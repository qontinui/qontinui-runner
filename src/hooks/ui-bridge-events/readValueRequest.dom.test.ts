// @vitest-environment jsdom
/**
 * The relay `read_value` request keeps the SDK's §4.6 redaction contract:
 * a password input and anything inside a `data-bridge-redact` boundary read
 * back `[REDACTED]`, a plain input reads its value.
 */

import { describe, it, expect, afterEach } from "vitest";
import { REDACTED_VALUE } from "@qontinui/ui-bridge";
import { runReadValueRequest } from "./readValueRequest";

afterEach(() => {
  document.body.innerHTML = "";
});

function mount(html: string): void {
  const host = document.createElement("div");
  host.innerHTML = html;
  document.body.appendChild(host);
}

describe("read_value §4.6 redaction", () => {
  it("withholds a password input and reads a plain one", () => {
    mount(`<input id="pw" type="password" value="hunter2"><input id="t" type="text" value="hello">`);
    const pw = runReadValueRequest({ selector: "#pw" });
    expect(pw).toEqual({
      success: true,
      data: { value: REDACTED_VALUE, length: 0, totalMatches: 1 },
    });
    expect(JSON.stringify(pw)).not.toContain("hunter2");
    const t = runReadValueRequest({ selector: "#t" });
    expect(t.success && t.data.value).toBe("hello");
  });

  it("withholds a value inside a data-bridge-redact boundary", () => {
    mount(`<section data-bridge-redact="true"><input id="s" type="text" value="tok-123"></section>`);
    const r = runReadValueRequest({ selector: "#s" });
    expect(r.success && r.data.value).toBe(REDACTED_VALUE);
    expect(JSON.stringify(r)).not.toContain("tok-123");
  });

  it("all:true applies the same rule per match, and index:null is tolerated", () => {
    mount(`<input class="r" type="password" value="p1"><input class="r" value="plain">`);
    const r = runReadValueRequest({ selector: ".r", all: true, index: null });
    expect(r.success).toBe(true);
    if (!r.success) return;
    expect(r.data.totalMatches).toBe(2);
    expect(r.data.values?.map((v) => v.value)).toEqual([REDACTED_VALUE, "plain"]);
    expect(JSON.stringify(r)).not.toContain("p1");
  });

  it("a miss and a bad selector are errors, not empty values", () => {
    expect(runReadValueRequest({ selector: "#nope" }).success).toBe(false);
    expect(runReadValueRequest({ selector: undefined }).success).toBe(false);
  });
});
