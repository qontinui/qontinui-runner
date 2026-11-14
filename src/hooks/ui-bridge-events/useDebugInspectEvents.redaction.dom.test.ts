// @vitest-environment jsdom
/**
 * The IPC handlers behind `GET /ui-bridge/control/element/{id}/tree` and
 * `GET /ui-bridge/debug/element-tree` withhold what the SDK's §4.6 verdict
 * withholds. Enters through the real `useDebugInspectEvents` body (React's
 * hooks stubbed to pass-throughs, as `useCommands.test.ts` does), so it covers
 * the route's own dispatch, not only the extracted walk.
 */

import { describe, it, expect, vi, afterEach } from "vitest";

vi.mock("react", () => ({
  useCallback: (fn: unknown) => fn,
}));

import { REDACTED_VALUE } from "@qontinui/ui-bridge";
import { useDebugInspectEvents } from "./useDebugInspectEvents";
import type { UIBridgeRequestPayload } from "./types";

afterEach(() => {
  document.body.innerHTML = "";
});

interface Sent {
  success: boolean;
  data?: Record<string, unknown>;
}

function useHandlerUnderTest(
  elements: Array<{ id: string; type: string; label?: string; element: Element }>,
) {
  const sent: Sent[] = [];
  const bridge = {
    elements: elements.map((e) => ({ ...e, getState: () => ({ visible: true }) })),
    discover: async () => ({ elements: [] }),
  };
  const handle = useDebugInspectEvents({
    bridgeRef: { current: bridge },
    sendResponse: async (r: Sent) => {
      sent.push(r);
    },
  } as unknown as Parameters<typeof useDebugInspectEvents>[0]);
  return { handle, sent };
}

describe("useDebugInspectEvents §4.6 redaction", () => {
  it("get_element_dom_tree scrubs a boundary descendant and a bare password input", async () => {
    const host = document.createElement("div");
    host.innerHTML = `
      <div data-bridge-redact="true"><span title="secret" data-foo="x" role="note"></span></div>
      <input type="password" aria-label="Password" data-hint="pw"/>`;
    document.body.appendChild(host);
    const { handle, sent } = useHandlerUnderTest([{ id: "root", type: "div", element: host }]);

    await handle({
      requestId: "r1",
      type: "get_element_dom_tree",
      params: { elementId: "root", depth: 3 },
    } as unknown as UIBridgeRequestPayload);

    expect(sent[0].success).toBe(true);
    const tree = sent[0].data!.tree as {
      children: Array<{
        attrs: Record<string, string>;
        children: Array<{ attrs: Record<string, string> }>;
      }>;
    };
    const span = tree.children[0].children[0].attrs;
    expect(span.title).toBe(REDACTED_VALUE);
    expect(span["data-foo"]).toBe(REDACTED_VALUE);
    expect(span.role).toBe("note");
    const input = tree.children[1].attrs;
    expect(input["data-hint"]).toBe(REDACTED_VALUE);
    expect(input["aria-label"]).toBe("Password");
  });

  it("get_element_tree collapses a label inside a boundary only", async () => {
    const host = document.createElement("div");
    host.innerHTML = `<div data-bridge-redact="true"><button id="in"></button></div><button id="out"></button>`;
    document.body.appendChild(host);
    const { handle, sent } = useHandlerUnderTest([
      { id: "in", type: "button", label: "Secret", element: host.querySelector("#in")! },
      { id: "out", type: "button", label: "Plain", element: host.querySelector("#out")! },
    ]);

    await handle({
      requestId: "r2",
      type: "get_element_tree",
    } as unknown as UIBridgeRequestPayload);

    const tree = sent[0].data!.tree as Array<{ id: string; label?: string }>;
    expect(tree.find((n) => n.id === "in")!.label).toBe(REDACTED_VALUE);
    expect(tree.find((n) => n.id === "out")!.label).toBe("Plain");
  });
});
