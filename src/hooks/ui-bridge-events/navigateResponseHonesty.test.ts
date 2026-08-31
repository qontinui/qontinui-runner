/**
 * `page_navigate` must not report a reload it did not perform.
 *
 * The documented contract said `mode: "hard"` (the DEFAULT) did a "full
 * webview reload via `window.location.href = url`", and the response asserted
 * `hard: true`. The handler dispatched a `ui-bridge-navigate` CustomEvent plus
 * `history.pushState` — a soft SPA navigation — and measurement caught the
 * gap: across four `hard` navigations the SDK's in-memory navigation ring kept
 * all 20 of its entries (oldest 43 minutes stale) and the single boot-time
 * `[PROJECT_SELECTION]` console error stayed single. A real reload resets both.
 *
 * The contract moved rather than the handler, because a reload here cannot
 * work: the runner has no URL router (navigation is a tab id through
 * `PAGE_TO_TAB`), the path is not an asset under the embedded Tauri asset
 * protocol, and `page_refresh` already refuses to reload for state-loss
 * reasons. See `ui_bridge_page_navigate_handler` in `page.rs`.
 */

import { describe, it, expect } from "vitest";

import { buildNavigateResponseData } from "./usePageEvents";

describe("buildNavigateResponseData", () => {
  it("does not claim a hard reload on the default mode", () => {
    const data = buildNavigateResponseData("/settings", "hard");
    expect(data.hard).toBe(false);
    expect(data.reloaded).toBe(false);
  });

  it("does not claim a reload on soft mode either", () => {
    const data = buildNavigateResponseData("/terminal", "soft");
    expect(data.hard).toBe(false);
    expect(data.reloaded).toBe(false);
  });

  it("still echoes the requested mode and url so callers can audit them", () => {
    expect(buildNavigateResponseData("/settings", "hard").mode).toBe("hard");
    expect(buildNavigateResponseData("/settings", "soft").mode).toBe("soft");
    expect(buildNavigateResponseData("/settings", "hard").url).toBe("/settings");
  });

  it("never lets the requested mode decide the outcome fields", () => {
    // The precise shape of the defect: `hard: mode === "hard"`. Whatever the
    // caller asks for, the reported outcome is identical, because the two
    // branches do the same non-reloading thing.
    const hard = buildNavigateResponseData("/settings", "hard");
    const soft = buildNavigateResponseData("/settings", "soft");
    expect(hard.hard).toBe(soft.hard);
    expect(hard.reloaded).toBe(soft.reloaded);
  });
});
