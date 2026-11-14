/**
 * `saveConstraintConfig`'s failure message: the backend refuses with JSON
 * `{error, code, hint}`, and the thrown message carries `error` rather than the
 * raw JSON body; a non-JSON body still surfaces as text.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const tracedFetch = vi.fn();
vi.mock("@/lib/runner-api", () => ({
  getApiBase: () => "http://127.0.0.1:1",
  tracedFetch: (...args: unknown[]) => tracedFetch(...args),
}));

import { saveConstraintConfig } from "./constraints-api";

describe("saveConstraintConfig", () => {
  beforeEach(() => tracedFetch.mockReset());

  it("throws the JSON body's `error` on the workspace_root_unresolved 422", async () => {
    tracedFetch.mockResolvedValue(
      new Response(
        JSON.stringify({
          success: false,
          error: "the workspace root did not resolve",
          code: "workspace_root_unresolved",
          hint: { code: "workspace_root_unresolved", next_action: "set QONTINUI_ROOT" },
        }),
        { status: 422, headers: { "Content-Type": "application/json" } },
      ),
    );
    await expect(saveConstraintConfig("x = 1")).rejects.toThrow(
      "Failed to save constraint config: the workspace root did not resolve",
    );
  });

  it("falls back to the raw text when the JSON `error` is empty", async () => {
    const body = JSON.stringify({ success: false, error: "", code: "workspace_root_unresolved" });
    tracedFetch.mockResolvedValue(new Response(body, { status: 422 }));
    await expect(saveConstraintConfig("x = 1")).rejects.toThrow(
      `Failed to save constraint config: ${body}`,
    );
  });

  it("falls back to the raw text when the body is not JSON", async () => {
    tracedFetch.mockResolvedValue(new Response("upstream exploded", { status: 500 }));
    await expect(saveConstraintConfig("x = 1")).rejects.toThrow(
      "Failed to save constraint config: upstream exploded",
    );
  });

  it("falls back to the raw text when the JSON has no string `error`", async () => {
    tracedFetch.mockResolvedValue(new Response(JSON.stringify({ code: "X" }), { status: 422 }));
    await expect(saveConstraintConfig("x = 1")).rejects.toThrow(
      'Failed to save constraint config: {"code":"X"}',
    );
  });
});
