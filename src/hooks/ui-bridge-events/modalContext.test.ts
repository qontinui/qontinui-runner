/**
 * `get_modal_context` IPC — the null contract.
 *
 * The Rust `/control/visibility` twin (`screenshots.rs`) reads a `null` answer
 * as `expectedOverlayDetection: "unavailable"` and an object carrying a
 * `modals` array (even an empty one) as `"modal-stack"`. So a failure shape
 * that leaked through as `{}` or `{ modals: [] }` would label "could not
 * classify" as "classified, nothing open", and a real empty stack collapsed to
 * `null` would do the reverse. Both directions are pinned here.
 */

import { describe, it, expect, vi } from "vitest";

import { readModalContext } from "./useDiscoveryEvents";

describe("readModalContext", () => {
  it("passes a real modal stack through untouched", () => {
    const ctx = { modals: [{ id: "modal-1", type: "dialog" }], topModal: null };
    expect(readModalContext({ registry: { getModalContext: () => ctx } })).toBe(ctx);
  });

  it("keeps an EMPTY stack as an answer, not as null", () => {
    const ctx = { modals: [], topModal: null };
    expect(readModalContext({ registry: { getModalContext: () => ctx } })).toBe(ctx);
  });

  describe("every failure shape answers null", () => {
    it.each([
      ["no bridge (null)", null],
      ["no bridge (undefined)", undefined],
      ["no registry", {}],
      ["no getModalContext accessor", { registry: {} }],
      ["accessor answering undefined", { registry: { getModalContext: () => undefined } }],
      ["accessor answering null", { registry: { getModalContext: () => null } }],
    ])("%s", (_label, bridge) => {
      expect(readModalContext(bridge)).toBeNull();
    });
  });

  it("a throwing tracker degrades to null instead of failing the request", () => {
    const getModalContext = vi.fn(() => {
      throw new Error("tracker exploded");
    });
    expect(readModalContext({ registry: { getModalContext } })).toBeNull();
    expect(getModalContext).toHaveBeenCalledOnce();
  });
});
