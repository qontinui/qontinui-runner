/**
 * Ownership-precedence tests for `WindowAssignmentsContext`.
 *
 * These lock the bug fixed by threading `pageId` into the ownership predicate:
 * in a page-bound pop-out window, every terminal WITHOUT a `session_owner`
 * entry rendered but was permanently stdin-dead — the renderer answered
 * "page-binding wins, else the owner map" while the stdin gate answered
 * "owner map only". Both now call the SAME predicate.
 *
 * The provider itself is not rendered: the runner's vitest config uses
 * `environment: "node"` (no jsdom — see `vitest.config.ts`), so
 * `render(<WindowAssignmentsProvider>)` isn't viable here. The decision worth
 * covering is a pure one — "given this `get_window_assignments` snapshot, does
 * window X own session S on page P" — so the test mocks the Tauri `invoke`,
 * pulls the snapshot exactly as the provider's `refresh()` does, and runs it
 * through the same two pure functions the provider composes
 * (`bindingsFromWindows` → `isOwnedBy`). That is the honest assertion, not a
 * proxy for one.
 */

import { describe, expect, it, vi } from "vitest";

interface Snapshot {
  session_owner: Record<string, string>;
  windows: Record<string, { label: string; kind: "main" | "pop_out"; bound_page: string | null }>;
}

/** Snapshot the mocked `get_window_assignments` returns. */
const SNAPSHOT: Snapshot = {
  session_owner: {
    // A terminal explicitly "sent to" a pop-out window (per-terminal move).
    "sess-sent-to-term-2": "term-2",
    // A terminal the owner map still attributes to main, whose PAGE has since
    // been detached into `term-1` — the double-send case.
    "sess-still-mapped-to-main": "main",
  },
  windows: {
    main: { label: "main", kind: "main" as const, bound_page: null },
    // Whole page `page-popped` detached into this window ("Pop Out Page").
    "term-1": { label: "term-1", kind: "pop_out" as const, bound_page: "page-popped" },
    // A pop-out that holds individual terminals, not a whole page.
    "term-2": { label: "term-2", kind: "pop_out" as const, bound_page: null },
  },
};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "get_window_assignments") return SNAPSHOT;
    return undefined;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ label: "main" }),
}));
vi.mock("@/lib/pageBindings", () => ({ setPageBindings: vi.fn() }));

// Imported after the mocks are registered (vi.mock is hoisted).
const { invoke } = await import("@tauri-apps/api/core");
const { MAIN_WINDOW_LABEL, bindingsFromWindows, isOwnedBy, ownedTabs, ownerOfSession } =
  await import("./WindowAssignmentsContext");

/** Read the snapshot the way the provider's `refresh()` does. */
async function loadState() {
  const snap = await invoke<Snapshot>("get_window_assignments");
  return {
    assignments: snap.session_owner,
    pageBindings: bindingsFromWindows(snap.windows),
  };
}

describe("bindingsFromWindows", () => {
  it("derives pageId → windowLabel from the window registry, skipping unbound windows", async () => {
    const { pageBindings } = await loadState();
    expect(pageBindings).toEqual({ "page-popped": "term-1" });
  });
});

describe("isOwnedBy — page-binding precedence", () => {
  it("a page bound to THIS window owns its tabs even with no session_owner entry", async () => {
    // REGRESSION: this is the stdin-dead bug. `sess-fresh` is absent from the
    // owner map (the normal state after a restart — `clear_session_owners()`
    // empties it — and for any tab created before the pop-out), so the
    // owner-map-only answer was "main owns it" and the pop-out's stdin gate
    // stayed shut forever while the tab still rendered.
    const { assignments, pageBindings } = await loadState();
    expect(assignments["sess-fresh"]).toBeUndefined();
    expect(isOwnedBy("term-1", assignments, pageBindings, "sess-fresh", "page-popped")).toBe(true);
    // ...and the main window must NOT also claim it (single writer).
    expect(isOwnedBy("main", assignments, pageBindings, "sess-fresh", "page-popped")).toBe(false);
  });

  it("a page bound to ANOTHER window is not owned here, even with a session_owner entry naming this window", async () => {
    // The double-send guard: during the transient double-mount the main window
    // still has the owner entry, but the binding already moved the page. An
    // unqualified "am I page-bound?" predicate would return true in BOTH
    // windows and double-send every keystroke.
    const { assignments, pageBindings } = await loadState();
    expect(ownerOfSession(assignments, "sess-still-mapped-to-main")).toBe(MAIN_WINDOW_LABEL);
    expect(
      isOwnedBy("main", assignments, pageBindings, "sess-still-mapped-to-main", "page-popped"),
    ).toBe(false);
    expect(
      isOwnedBy("term-1", assignments, pageBindings, "sess-still-mapped-to-main", "page-popped"),
    ).toBe(true);
  });

  it("an unbound page falls back to the per-terminal owner map", async () => {
    const { assignments, pageBindings } = await loadState();
    expect(
      isOwnedBy("term-2", assignments, pageBindings, "sess-sent-to-term-2", "page-docked"),
    ).toBe(true);
    expect(isOwnedBy("main", assignments, pageBindings, "sess-sent-to-term-2", "page-docked")).toBe(
      false,
    );
  });

  it("an unbound page with no owner entry belongs to the main window", async () => {
    const { assignments, pageBindings } = await loadState();
    expect(isOwnedBy("main", assignments, pageBindings, "sess-fresh", "page-docked")).toBe(true);
    expect(isOwnedBy("term-1", assignments, pageBindings, "sess-fresh", "page-docked")).toBe(false);
  });

  it("renders in the pop-out exactly the tabs whose stdin it gates", async () => {
    // The RENDERER half of "one predicate, one precedence". `ownedTabs` is what
    // `PageSessionScope`'s `tabs` memo calls, so this is the guard against the
    // mirror-image regression: drop `pageId` here and a page-bound pop-out
    // renders NOTHING, while the stdin gate still claims the tabs.
    const { assignments, pageBindings } = await loadState();
    const allTabs = [{ id: "sess-fresh" }, { id: "sess-still-mapped-to-main" }];
    const ownedBy = (label: string) => (sessionId: string, pageId?: string | null) =>
      isOwnedBy(label, assignments, pageBindings, sessionId, pageId);

    // Bound + mine → ALL of the page's tabs, owner map irrelevant.
    expect(ownedTabs(allTabs, ownedBy("term-1"), "page-popped")).toEqual(allTabs);
    // Bound + not mine → none, even though main still owns one in the map.
    expect(ownedTabs(allTabs, ownedBy("main"), "page-popped")).toEqual([]);
    // Unbound → falls back to the per-terminal owner map.
    expect(ownedTabs(allTabs, ownedBy("main"), "page-docked")).toEqual(allTabs);
    expect(ownedTabs(allTabs, ownedBy("term-2"), "page-docked")).toEqual([]);
  });

  it("omitting pageId yields the legacy owner-map-only answer", async () => {
    const { assignments, pageBindings } = await loadState();
    // Same session, same bound page — but without the pageId hint the binding
    // is invisible and the pop-out loses the tab. This is exactly the answer
    // the stdin gate used to get.
    expect(isOwnedBy("term-1", assignments, pageBindings, "sess-fresh")).toBe(false);
    expect(isOwnedBy("main", assignments, pageBindings, "sess-fresh")).toBe(true);
    expect(isOwnedBy("main", assignments, pageBindings, "sess-fresh", null)).toBe(true);
  });
});
