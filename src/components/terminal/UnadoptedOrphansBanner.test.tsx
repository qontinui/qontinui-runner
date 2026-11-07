// @vitest-environment jsdom
/**
 * The unadopted-orphans banner as rendered: shown when the adoption pass gave
 * up, withdrawn when the list empties, and — dismissed — kept dismissed while
 * the list only shrinks, shown again only for an orphan not yet shown.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { UnadoptedOrphansBanner } from "./UnadoptedOrphansBanner";
import type { UnadoptedOrphans } from "./useTerminalInitialization";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function show(entries: readonly UnadoptedOrphans[]) {
  act(() => root.render(createElement(UnadoptedOrphansBanner, { entries })));
}
const alert = () => container.querySelector('[role="alert"]');
const dismiss = () =>
  act(() => (container.querySelector('button[aria-label="Dismiss"]') as HTMLButtonElement).click());

describe("UnadoptedOrphansBanner", () => {
  it("names the orphans, and is withdrawn once the list empties", () => {
    show([{ removedPageIds: ["page-a"], sessionIds: ["s-1", "s-2"] }]);
    expect(alert()?.textContent).toContain("orphaned sessions s-1, s-2");
    show([]);
    expect(container.innerHTML).toBe("");
  });

  it("stays dismissed while the list only shrinks, and shows again for a new orphan", () => {
    show([{ removedPageIds: ["page-a"], sessionIds: ["s-1", "s-2"] }]);
    dismiss();
    expect(alert()).toBeNull();

    // s-1 adopted: a new entry object, no new orphan.
    show([{ removedPageIds: ["page-a"], sessionIds: ["s-2"] }]);
    expect(alert()).toBeNull();

    // A give-up over another page's orphan is news.
    show([
      { removedPageIds: ["page-a"], sessionIds: ["s-2"] },
      { removedPageIds: ["page-b"], sessionIds: ["s-9"] },
    ]);
    expect(alert()?.textContent).toContain("orphaned session s-9");
  });

  it("an orphan that left the list and is given up on again is shown, though others stay listed", () => {
    show([{ removedPageIds: ["page-a"], sessionIds: ["s-1", "s-2"] }]);
    dismiss();
    show([{ removedPageIds: ["page-a"], sessionIds: ["s-2"] }]);
    expect(alert()).toBeNull();
    show([
      { removedPageIds: ["page-a"], sessionIds: ["s-2"] },
      { removedPageIds: ["page-c"], sessionIds: ["s-1"] },
    ]);
    expect(alert()?.textContent).toContain("orphaned session s-1");
  });

  it("after a withdrawal, the same orphans given up on again are shown", () => {
    show([{ removedPageIds: ["page-a"], sessionIds: null }]);
    dismiss();
    show([]);
    show([{ removedPageIds: ["page-a"], sessionIds: null }]);
    expect(alert()?.textContent).toContain("could not name");
  });
});
