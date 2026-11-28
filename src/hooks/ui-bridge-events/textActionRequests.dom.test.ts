// @vitest-environment jsdom
/**
 * The relay find_by_text / click_by_text / click_by_selector requests keep the
 * SDK's §4.6 contract: text inside a `data-bridge-redact` boundary is never
 * matched or echoed.
 */

import { describe, it, expect, afterEach, vi } from "vitest";
import { REDACTED_VALUE } from "@qontinui/ui-bridge";
import {
  runFindByTextRequest,
  runClickByTextRequest,
  runClickBySelectorRequest,
  scrubbedTextContent,
  scrubbedAttribute,
} from "./textActionRequests";

// jsdom ships no `CSS.escape`, which the SDK's text matcher calls.
if (typeof globalThis.CSS === "undefined") {
  (globalThis as unknown as { CSS: { escape: (v: string) => string } }).CSS = {
    escape: (v: string) => v.replace(/[^a-zA-Z0-9_-]/g, "\\$&"),
  };
}

afterEach(() => {
  document.body.innerHTML = "";
});

function mount(html: string): void {
  const host = document.createElement("div");
  host.innerHTML = html;
  document.body.appendChild(host);
  // jsdom has no layout engine.
  HTMLElement.prototype.scrollIntoView = vi.fn();
}

describe("find_by_text §4.6", () => {
  it("drops a hit inside a redaction boundary and keeps a plain one", () => {
    mount(
      `<button id="a">Open sesame</button><section data-bridge-redact="true"><button id="b">Open sesame secret-9</button></section>`,
    );
    const r = runFindByTextRequest({ text: "open sesame", tag: "button" });
    expect(r.success).toBe(true);
    if (!r.success) return;
    expect(r.data).toHaveLength(1);
    expect(JSON.stringify(r)).not.toContain("secret-9");
  });

  it("requires text", () => {
    expect(runFindByTextRequest({})).toMatchObject({ success: false, code: "INVALID_PARAMS" });
  });
});

describe("click_by_text §4.6", () => {
  it("clicks a plain element and echoes its text", () => {
    mount(`<button id="a">Go now</button>`);
    const onClick = vi.fn();
    document.getElementById("a")!.addEventListener("click", onClick);
    const r = runClickByTextRequest({ text: "go now", tag: "button" });
    expect(onClick).toHaveBeenCalledTimes(1);
    expect(r.success && r.data.element.text).toBe("Go now");
  });

  it("never echoes text from inside a boundary", () => {
    mount(`<section data-bridge-redact="true"><button>Reveal token-7</button></section>`);
    const r = runClickByTextRequest({ text: "reveal", tag: "button" });
    expect(JSON.stringify(r)).not.toContain("token-7");
  });

  it("a miss is an ELEMENT_NOT_FOUND error", () => {
    expect(runClickByTextRequest({ text: "nothing here" })).toMatchObject({
      success: false,
      code: "ELEMENT_NOT_FOUND",
    });
  });
});

describe("click_by_selector §4.6", () => {
  it("scrubs the echo of a clicked element inside a boundary", () => {
    mount(`<section data-bridge-redact="true"><button id="s">pw-hunter2</button></section>`);
    const r = runClickBySelectorRequest({ selector: "#s", index: null });
    expect(r.success).toBe(true);
    expect(JSON.stringify(r)).not.toContain("hunter2");
    expect(r.success && r.data.element.text).toBe(REDACTED_VALUE);
  });

  it("requires a selector", () => {
    expect(runClickBySelectorRequest({})).toMatchObject({ success: false, code: "INVALID_PARAMS" });
  });
});

describe("scrub helpers", () => {
  it("scrubbedTextContent / scrubbedAttribute redact inside a boundary only", () => {
    mount(
      `<p id="p" aria-label="plain">visible</p><section data-bridge-redact="true"><p id="q" aria-label="secret-aria">secret</p></section>`,
    );
    const p = document.getElementById("p")!;
    const q = document.getElementById("q")!;
    expect(scrubbedTextContent(p)).toBe("visible");
    expect(scrubbedTextContent(q)).toBe(REDACTED_VALUE);
    expect(scrubbedAttribute(p, "aria-label")).toBe("plain");
    expect(scrubbedAttribute(q, "aria-label")).toBe(REDACTED_VALUE);
    expect(scrubbedAttribute(p, "title")).toBeUndefined();
  });
});
