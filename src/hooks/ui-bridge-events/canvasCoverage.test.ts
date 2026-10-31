/**
 * `get_canvas_coverage` IPC — the null / throw-marker contract.
 *
 * The Rust `/control/visibility` twin (`visibility_canvas.rs`) reads `null` as
 * coverage `unavailable` / `page-sdk-lacks-coverage`, the throw marker as
 * `accessor-threw`, and anything else as the census. The split mirrors the
 * SDK's in-page handler (`inPageCanvasCoverage`): no accessor at all is
 * `page-sdk-lacks-coverage`; an accessor that answers `undefined` (which is
 * what the SDK registry's own catch returns) is `accessor-threw`. All of them withhold a
 * `clear` verdict except a census with no unattributed canvas, so a failure
 * that leaked through as `{ canvases: 0, attributed: 0, unattributed: [] }`
 * would mint a false `clear`. Both directions are pinned here.
 */

import { describe, it, expect, vi } from "vitest";

import { readCanvasCoverage } from "./useDiscoveryEvents";

describe("readCanvasCoverage", () => {
  it("passes a real census through untouched", () => {
    const census = {
      canvases: 2,
      attributed: 1,
      unattributed: [{ descriptor: "canvas.stray", rect: { x: 0, y: 0, width: 64, height: 64 } }],
    };
    expect(readCanvasCoverage({ registry: { getCanvasCoverage: () => census } })).toBe(census);
  });

  it("keeps an EMPTY census as an answer, not as null", () => {
    const census = { canvases: 0, attributed: 0, unattributed: [] };
    expect(readCanvasCoverage({ registry: { getCanvasCoverage: () => census } })).toBe(census);
  });

  it("calls the accessor with the registry as `this`", () => {
    const registry = {
      census: { canvases: 0, attributed: 0, unattributed: [] },
      getCanvasCoverage(this: { census: unknown }) {
        return this.census;
      },
    };
    expect(readCanvasCoverage({ registry })).toBe(registry.census);
  });

  describe("every missing-accessor shape answers null", () => {
    it.each([
      ["no bridge (null)", null],
      ["no bridge (undefined)", undefined],
      ["no registry", {}],
      ["no getCanvasCoverage accessor (pre-0.36 SDK)", { registry: {} }],
      ["a non-function getCanvasCoverage", { registry: { getCanvasCoverage: 42 } }],
    ])("%s", (_label, bridge) => {
      expect(readCanvasCoverage(bridge)).toBeNull();
    });
  });

  describe("an accessor that answers nothing is accessor-threw, not a missing accessor", () => {
    it.each([
      ["accessor answering undefined (the SDK registry's own catch)", () => undefined],
      ["accessor answering null", () => null],
    ])("%s", (_label, getCanvasCoverage) => {
      expect(readCanvasCoverage({ registry: { getCanvasCoverage } })).toEqual({
        state: "unavailable",
        reason: "accessor-threw",
      });
    });
  });

  it("a throwing accessor answers the accessor-threw marker instead of failing the request", () => {
    const getCanvasCoverage = vi.fn(() => {
      throw new Error("census exploded");
    });
    expect(readCanvasCoverage({ registry: { getCanvasCoverage } })).toEqual({
      state: "unavailable",
      reason: "accessor-threw",
    });
    expect(getCanvasCoverage).toHaveBeenCalledOnce();
  });
});
