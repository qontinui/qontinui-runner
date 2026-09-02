/**
 * Regression tests for the two IPC projections in `useControlEvents` that
 * silently lost data.
 *
 * The vitest config is `environment: "node"` (no jsdom, no
 * `@testing-library/react`), so — following the precedent in
 * `useTerminalsEvents.test.ts` and `usePageEvents.test.ts` — the load-bearing
 * pieces are exported as pure functions and exercised directly rather than
 * through the hook wiring.
 */

import { describe, it, expect } from "vitest";
import type { StableRefResolution } from "@qontinui/ui-bridge";

import { toActionRequest, stableRefResponseData } from "./useControlEvents";

describe("toActionRequest", () => {
  /**
   * THE REGRESSION. The handler used to call
   * `executeAction(id, { action, params, waitOptions })` — a three-field copy
   * of a request grammar with a dozen fields — so every per-request opt-in the
   * Rust layer threaded into the IPC payload was dropped here, and the careful
   * forwarding in `sdk_client.rs` / `elements.rs` was dead code as shipped.
   */
  it("forwards the envelope WHOLE, by identity", () => {
    const envelope = {
      action: "click",
      params: { text: "hi" },
      waitOptions: { timeout: 500 },
      verifyEffect: true,
      fromSnapshotId: "ubs2_2_2_65a59daceda26fb2_c5d41be145c82269",
      includeResolutionAlternates: true,
    };

    const forwarded = toActionRequest(envelope);

    // Identity, not a deep-equal copy: a future field-by-field rebuild would
    // still deep-equal this fixture for the fields it happened to name, and
    // would still drop the next one. Identity is the property that cannot be
    // satisfied by an enumeration.
    expect(forwarded).toBe(envelope);
  });

  it("carries a field this file has never heard of", () => {
    // Stands in for the opt-in nobody has invented yet — the one an
    // enumerating rebuild is guaranteed to drop.
    const envelope = { action: "click", unknownFutureOptIn: { nested: [1, 2] } };
    expect(toActionRequest(envelope)).toBe(envelope);
    expect(toActionRequest(envelope)).toHaveProperty("unknownFutureOptIn");
  });

  it("builds an envelope for the bare-verb proxy-fallback form", () => {
    // The ONE input with nothing to forward: there is no envelope, only a verb.
    expect(toActionRequest("click")).toEqual({ action: "click" });
  });
});

describe("stableRefResponseData", () => {
  function resolution(id: string, mounted: boolean): StableRefResolution {
    return {
      element: { id, mounted } as StableRefResolution["element"],
      resolution: {
        strategy: "registry-id",
        stability: "exact",
      } as unknown as StableRefResolution["resolution"],
    };
  }

  /**
   * THE REGRESSION. `resolveStableRef` returns
   * `StableRefResolution { element, resolution }`; this projection still read
   * `resolved.id` / `resolved.mounted`. Both are `undefined` on the new shape,
   * so a SUCCESSFUL resolution answered `{elementId: undefined}` — which
   * `elements.rs`'s stable-ref retry (it reads `elementId` as a string) cannot
   * tell apart from a miss. The recovery path was dead for every hit.
   */
  it("reads the id and mount flag off the resolved ELEMENT", () => {
    const data = stableRefResponseData(resolution("btn_save", true));
    expect(data.elementId).toBe("btn_save");
    expect(data.mounted).toBe(true);
    // Never `undefined`: that is precisely the value that made a hit look like
    // a miss on the wire.
    expect(data.elementId).not.toBeUndefined();
  });

  it("passes the winning strategy through so the retry knows what it acted on", () => {
    const data = stableRefResponseData(resolution("btn_save", true));
    expect(data.resolution).toEqual({ strategy: "registry-id", stability: "exact" });
  });

  it("reports a genuine miss as an explicit null", () => {
    // `null`, not `undefined` — a miss must be a stated outcome, and it must
    // stay distinguishable from the hit case above.
    expect(stableRefResponseData(null)).toEqual({ elementId: null });
  });
});
