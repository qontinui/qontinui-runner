/**
 * `text_visibility` reaches the text-visibility engine through the page-events
 * dispatcher itself (`usePageEvents`' `case "text_visibility"`), and its reply
 * goes back through `sendResponse` carrying the request's id and type.
 */

import { describe, it, expect, vi, afterEach } from "vitest";

// The hook is a `useCallback`; outside a React render it is the callback itself.
vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useCallback: <T>(fn: T) => fn,
}));

import { usePageEvents } from "./usePageEvents";
import type { UIBridgeEventContext, UIBridgeRequestPayload } from "./types";

const KEY = Symbol.for("@qontinui/ui-bridge/dev-controls");

afterEach(() => {
  delete (globalThis as unknown as Record<symbol, unknown>)[KEY];
});

// Named as a hook: it calls one (outside a render, per the mock above).
function useDispatcher() {
  const sendResponse = vi.fn(async () => {});
  const context = {
    bridgeRef: { current: {} },
    sendResponse,
  } as unknown as Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">;
  return { handle: usePageEvents(context), sendResponse };
}

describe("usePageEvents text_visibility dispatch", () => {
  it("routes text_visibility to the engine and answers the typed refusal", async () => {
    const { handle, sendResponse } = useDispatcher();
    const handled = await handle({
      requestId: "r-1",
      type: "text_visibility",
      hidden: true,
    } as unknown as UIBridgeRequestPayload);
    expect(handled).toBe(true);
    expect(sendResponse).toHaveBeenCalledTimes(1);
    expect(sendResponse.mock.calls[0][0]).toMatchObject({
      requestId: "r-1",
      type: "text_visibility",
      success: false,
      data: { code: "capability_unavailable_in_build", reason: "dev_controls_disabled" },
    });
  });

  it("an opted-in realm gets the SDK engine's own answer", async () => {
    const { enableDevControls } = await import("@qontinui/ui-bridge/core");
    enableDevControls("provider-feature");
    const { handle, sendResponse } = useDispatcher();
    await handle({
      requestId: "r-2",
      type: "text_visibility",
      hidden: false,
    } as unknown as UIBridgeRequestPayload);
    expect(sendResponse.mock.calls[0][0]).toMatchObject({
      requestId: "r-2",
      type: "text_visibility",
      success: true,
      data: { success: true, hidden: false, changed: false },
    });
  });
});
