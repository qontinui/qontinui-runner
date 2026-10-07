// @vitest-environment jsdom
/**
 * Tests for the set-tab page-id read-back walk that
 * src-tauri/src/mcp/ui_bridge/page.rs embeds (include_str!) into the
 * POST /ui-bridge/control/page/set-tab eval expression. The file is loaded
 * verbatim, so these tests exercise exactly what the webview runs.
 *
 * jsdom has no layout, so every element reports a zero rect; visibility is
 * stubbed per element: anything inside `data-test-visible="false"` has a
 * zero rect.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, describe, expect, it } from "vitest";

interface Readback {
  pageId: string | null;
  activePageId: string | null;
  pageIdChain: string[];
}

const source = readFileSync(
  resolve(__dirname, "../../../../src-tauri/src/mcp/ui_bridge/set_tab_readback.js"),
  "utf8",
);
const readSetTabPageIds = new Function(`${source}\nreturn readSetTabPageIds;`)() as (
  doc: Document,
) => Readback;

function render(html: string): void {
  document.body.innerHTML = html;
  for (const el of Array.from(document.querySelectorAll<HTMLElement>("*"))) {
    // display:none hides the whole subtree, so an ancestor's flag applies too.
    const visible = el.closest('[data-test-visible="false"]') === null;
    el.getBoundingClientRect = () =>
      ({
        width: visible ? 100 : 0,
        height: visible ? 100 : 0,
      }) as DOMRect;
  }
}

describe("readSetTabPageIds", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("reports the deepest visible nested page id and its chain", () => {
    render(`
      <div data-page-id="productivity">
        <section>
          <div data-page-id="productivity-calendar"><span></span></div>
        </section>
      </div>`);
    expect(readSetTabPageIds(document)).toEqual({
      pageId: "productivity",
      activePageId: "productivity-calendar",
      pageIdChain: ["productivity", "productivity-calendar"],
    });
  });

  it("deepest wins over a shallower match later in the document", () => {
    render(`
      <div data-page-id="outer">
        <div><div><div data-page-id="deep"></div></div></div>
        <div data-page-id="shallow"></div>
      </div>`);
    expect(readSetTabPageIds(document).activePageId).toBe("deep");
  });

  it("breaks depth ties toward the last element in document order", () => {
    // Guards the `>=` comparison: with `>` the first sibling would win.
    render(`
      <div data-page-id="outer">
        <div data-page-id="first"></div>
        <div data-page-id="second"></div>
      </div>`);
    expect(readSetTabPageIds(document).activePageId).toBe("second");
  });

  it("skips zero-rect (unmounted / display:none) views", () => {
    render(`
      <div data-page-id="outer">
        <div data-page-id="visible"></div>
        <div data-test-visible="false"><div data-page-id="hidden-deep"></div></div>
      </div>`);
    const r = readSetTabPageIds(document);
    expect(r.activePageId).toBe("visible");
    expect(r.pageIdChain).toEqual(["outer", "visible"]);
  });

  it("falls back to the wrapper when the page publishes no nested id", () => {
    render(`<div data-page-id="settings"><div><button>General</button></div></div>`);
    expect(readSetTabPageIds(document)).toEqual({
      pageId: "settings",
      activePageId: "settings",
      pageIdChain: ["settings"],
    });
  });

  it("ignores page ids outside the first wrapper", () => {
    render(`
      <div data-page-id="outer"><div data-page-id="inside"></div></div>
      <aside><div><div><div data-page-id="stray-deeper"></div></div></div></aside>`);
    expect(readSetTabPageIds(document).activePageId).toBe("inside");
  });

  it("reports null/empty when nothing carries a page id", () => {
    render(`<div><span>no pages</span></div>`);
    expect(readSetTabPageIds(document)).toEqual({
      pageId: null,
      activePageId: null,
      pageIdChain: [],
    });
  });

  it("reports pageId but no active view when the wrapper itself is hidden", () => {
    render(`
      <div data-page-id="outer" data-test-visible="false">
        <div data-page-id="inner"></div>
      </div>`);
    expect(readSetTabPageIds(document)).toEqual({
      pageId: "outer",
      activePageId: null,
      pageIdChain: [],
    });
  });

  it("collapses consecutive duplicate ids and orders the chain outer to inner", () => {
    render(`
      <div data-page-id="projects">
        <div data-page-id="projects">
          <div><div data-page-id="project-detail"></div></div>
        </div>
      </div>`);
    expect(readSetTabPageIds(document).pageIdChain).toEqual(["projects", "project-detail"]);
  });
});
