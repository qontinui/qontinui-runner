// @vitest-environment jsdom
/**
 * `/tree` and `/debug/element-tree` keep the SDK's §4.6 redaction contract:
 * what the element read withholds, these reads withhold too.
 */

import { describe, it, expect, afterEach } from "vitest";
import { REDACTED_VALUE } from "@qontinui/ui-bridge";
import { buildElementDomTree, debugTreeLabel, serializeDomTreeAttrs } from "./elementDomTree";

afterEach(() => {
  document.body.innerHTML = "";
});

function mount(html: string): HTMLElement {
  const host = document.createElement("div");
  host.innerHTML = html;
  document.body.appendChild(host);
  return host;
}

describe("elementDomTree §4.6 scrub", () => {
  it("redacts content and value keys inside a boundary, keeps structural keys raw", () => {
    const host = mount(`
      <section id="b" data-bridge-redact="true">
        <button id="c" role="tab" name="n" type="button" class="btn"
          title="secret title" aria-label="secret label"
          data-foo="secret-data" data-testid="tid" data-ui-bridge-id="uid"
          data-ui-bridge-test-id="utid" data-awas-element="awas" data-bridge-kind="k"></button>
      </section>`);
    const attrs = serializeDomTreeAttrs(host.querySelector("#c")!);
    expect(attrs).toEqual({
      role: "tab",
      name: "n",
      type: "button",
      class: "btn",
      title: REDACTED_VALUE,
      "aria-label": REDACTED_VALUE,
      "data-foo": REDACTED_VALUE,
      "data-testid": "tid",
      "data-ui-bridge-id": "uid",
      "data-ui-bridge-test-id": "utid",
      "data-awas-element": "awas",
      "data-bridge-kind": "k",
    });
  });

  it("redacts the boundary element's own title, keeps its data-bridge-redact", () => {
    const host = mount(`<section id="b" data-bridge-redact="true" title="t"></section>`);
    const attrs = serializeDomTreeAttrs(host.querySelector("#b")!);
    expect(attrs.title).toBe(REDACTED_VALUE);
    expect(attrs["data-bridge-redact"]).toBe("true");
  });

  it("redacts an SVG child inside a boundary through the walk", () => {
    const host = mount(`
      <div id="root" data-bridge-redact="true">
        <svg aria-label="chart secret" data-series="s"><title>x</title></svg>
      </div>`);
    const { tree } = buildElementDomTree(host.querySelector("#root")!, 3);
    const svg = tree.children[0];
    expect(svg.tagName.toLowerCase()).toBe("svg");
    expect(svg.attrs["aria-label"]).toBe(REDACTED_VALUE);
    expect(svg.attrs["data-series"]).toBe(REDACTED_VALUE);
  });

  it("serves everything raw outside a boundary", () => {
    const host = mount(
      `<div id="root"><span title="t" aria-label="a" data-foo="f" role="note"></span></div>`,
    );
    const { tree } = buildElementDomTree(host.querySelector("#root")!, 3);
    expect(tree.children[0].attrs).toEqual({
      title: "t",
      "aria-label": "a",
      "data-foo": "f",
      role: "note",
    });
  });

  it("redacts a bare password input's data-* on the VALUE axis, keeps its aria-label", () => {
    const host = mount(
      `<form id="root"><input type="password" aria-label="Password" data-foo="pw-hint"/></form>`,
    );
    const { tree } = buildElementDomTree(host.querySelector("#root")!, 3);
    const input = tree.children[0].attrs;
    expect(input["data-foo"]).toBe(REDACTED_VALUE);
    expect(input["aria-label"]).toBe("Password");
    expect(input.type).toBe("password");
  });

  it("keeps the walk's shape: depth, cap and truncation", () => {
    const host = mount(`<ul id="root">${"<li><b></b></li>".repeat(250)}</ul>`);
    const out = buildElementDomTree(host.querySelector("#root")!, 1);
    expect(out.depth).toBe(1);
    expect(out.truncated).toBe(true);
    expect(out.tree.children).toHaveLength(199);
    expect(out.tree.childCount).toBe(250);
  });

  it("collapses the /debug/element-tree label inside a boundary only", () => {
    const host = mount(`
      <div data-bridge-redact="true"><button id="in">x</button></div>
      <button id="out">y</button>`);
    expect(debugTreeLabel(host.querySelector("#in"), "Secret label")).toBe(REDACTED_VALUE);
    expect(debugTreeLabel(host.querySelector("#out"), "Plain label")).toBe("Plain label");
    expect(debugTreeLabel(host.querySelector("#in"), undefined)).toBeUndefined();
  });
});
