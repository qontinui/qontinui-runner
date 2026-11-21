// @vitest-environment jsdom
/**
 * `read_value` against a real (jsdom) document: the redaction verdict
 * (§4.6) applies to every read. A `type="password"` input and any value inside
 * a `data-bridge-redact="true"` boundary read back `[REDACTED]` with length 0;
 * a plain input reads its value. Pins plan
 * 2026-11-13-the-runners-read-value-serves-password-values-raw: the runner's
 * own inline copies of this read served both in cleartext.
 */

import { describe, it, expect, afterEach } from "vitest";
import { runReadValueRequest } from "./readValueRequest";

const SECRET = "hunter2-the-stored-auth-token";

function mount(html: string): void {
  document.body.innerHTML = html;
  // jsdom reflects the `value` attribute into `.value`, but set it explicitly
  // so the secret is unmistakably the live value being read.
  for (const el of Array.from(document.querySelectorAll<HTMLInputElement>("input"))) {
    el.value = el.getAttribute("data-v") ?? "";
  }
}

afterEach(() => {
  document.body.innerHTML = "";
});

describe("read_value redaction (DOM)", () => {
  it("reads a type=password input back as [REDACTED] with length 0", () => {
    mount(`<input id="pw" type="password" data-v="${SECRET}">`);
    const reply = runReadValueRequest({ selector: "#pw" });
    expect(reply.success).toBe(true);
    if (!reply.success) return;
    expect(reply.data).toMatchObject({ value: "[REDACTED]", length: 0, totalMatches: 1 });
    expect(JSON.stringify(reply)).not.toContain(SECRET);
  });

  it("reads an input inside data-bridge-redact=true back as [REDACTED]", () => {
    mount(`<div data-bridge-redact="true"><input id="t" type="text" data-v="${SECRET}"></div>`);
    const reply = runReadValueRequest({ selector: "#t" });
    expect(reply.success).toBe(true);
    if (!reply.success) return;
    expect(reply.data).toMatchObject({ value: "[REDACTED]", length: 0 });
    expect(JSON.stringify(reply)).not.toContain(SECRET);
  });

  it("reads a plain input's value", () => {
    mount(`<input id="n" type="text" data-v="plain-value">`);
    const reply = runReadValueRequest({ selector: "#n" });
    expect(reply).toEqual({
      success: true,
      data: { value: "plain-value", length: 11, totalMatches: 1 },
    });
  });

  it("all:true returns one entry per match and applies the rule to each", () => {
    mount(`
      <input class="r" type="text" data-v="open">
      <input class="r" type="password" data-v="${SECRET}">
      <div data-bridge-redact="true"><input class="r" type="text" data-v="${SECRET}"></div>
    `);
    const reply = runReadValueRequest({ selector: "input.r", all: true });
    expect(reply.success).toBe(true);
    if (!reply.success) return;
    const data = reply.data as {
      totalMatches: number;
      values: Array<{ index: number; value: string | null; length: number }>;
    };
    expect(data.totalMatches).toBe(3);
    expect(data.values).toEqual([
      { index: 0, value: "open", length: 4 },
      { index: 1, value: "[REDACTED]", length: 0 },
      { index: 2, value: "[REDACTED]", length: 0 },
    ]);
    expect(JSON.stringify(reply)).not.toContain(SECRET);
  });

  it("treats a wire null index as absent: {all:true,index:null} works", () => {
    mount(`<input class="r" data-v="a"><input class="r" data-v="b">`);
    const reply = runReadValueRequest({ selector: "input.r", all: true, index: null });
    expect(reply.success).toBe(true);
    if (!reply.success) return;
    expect((reply.data as { values: unknown[] }).values).toHaveLength(2);
  });

  it("treats a wire null all as absent and honours index", () => {
    mount(`<input class="r" data-v="a"><input class="r" data-v="b">`);
    const reply = runReadValueRequest({ selector: "input.r", all: null, index: 1 });
    expect(reply).toEqual({
      success: true,
      data: { value: "b", length: 1, totalMatches: 2 },
    });
  });

  it("answers the primitive's error for a miss, a bad all, and all+index", () => {
    mount(`<input class="r" data-v="a">`);
    const miss = runReadValueRequest({ selector: "#nope" });
    expect(miss).toMatchObject({ success: false, code: "ELEMENT_NOT_FOUND" });
    const badAll = runReadValueRequest({ selector: "input", all: "yes" });
    expect(badAll).toMatchObject({ success: false, code: "INVALID_PARAMS" });
    const both = runReadValueRequest({ selector: "input", all: true, index: 0 });
    expect(both).toMatchObject({ success: false, code: "INVALID_PARAMS" });
    const empty = runReadValueRequest({ selector: "" });
    expect(empty).toMatchObject({ success: false, code: "INVALID_PARAMS" });
  });
});
