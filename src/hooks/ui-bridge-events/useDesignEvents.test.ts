// @vitest-environment jsdom
/**
 * useDesignEvents answers the design commands through ui-bridge's shared
 * request runtime (plan
 * `2026-11-15-runner-design-events-adopt-the-shared-ui-bridge-request-runtime`,
 * Phase 2).
 *
 * The Rust routes wrap every HTTP body as `{params: body}`, so these requests
 * carry their fields under `params` and nowhere else — the shape that used to
 * drop a caller's `viewports` on `/design/responsive`. The runtime functions
 * are the real ones, wrapped in spies so a test can see what reached them.
 *
 * No renderer: `useCallback` / `useRef` are stubbed so the hook can be called
 * directly and hand back its handler.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useCallback: <T>(fn: T) => fn,
  useRef: <T>(initial: T) => ({ current: initial }),
}));
vi.mock("@qontinui/ui-bridge", async (importOriginal) => {
  const real = await importOriginal<typeof import("@qontinui/ui-bridge")>();
  return {
    ...real,
    responsiveSnapshotsOf: vi.fn(real.responsiveSnapshotsOf),
    designAuditOf: vi.fn(real.designAuditOf),
    designSnapshotOf: vi.fn(real.designSnapshotOf),
    evaluateRegisteredQuality: vi.fn(real.evaluateRegisteredQuality),
  };
});

import {
  designAuditOf,
  designSnapshotOf,
  evaluateRegisteredQuality,
  responsiveSnapshotsOf,
} from "@qontinui/ui-bridge";
import type { StyleGuideConfig } from "@qontinui/ui-bridge";
import type {
  RegisteredElement,
  UIBridgeEventContext,
  UIBridgeRequestPayload,
  UIBridgeResponsePayload,
} from "./types";
import { useDesignEvents } from "./useDesignEvents";

const GUIDE: StyleGuideConfig = {
  name: "loaded-guide",
  version: "1.0.0",
  rules: [],
} as unknown as StyleGuideConfig;

function setup(elements: RegisteredElement[], loadedGuide: StyleGuideConfig | null = null) {
  const responses: UIBridgeResponsePayload[] = [];
  const bridge = {
    elements,
    getElement: (id: string) => elements.find((el) => el.id === id),
  };
  const context = {
    bridgeRef: { current: bridge },
    loadedStyleGuideRef: { current: loadedGuide },
    sendResponse: async (response: UIBridgeResponsePayload) => {
      responses.push(response);
    },
  } as unknown as Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse" | "loadedStyleGuideRef">;
  // eslint-disable-next-line react-hooks/rules-of-hooks -- no renderer here; the hooks are stubbed above
  const handle = useDesignEvents(context);
  const send = async (type: string, params?: Record<string, unknown>) => {
    const handled = await handle({ requestId: "req-1", type, params } as UIBridgeRequestPayload);
    expect(handled).toBe(true);
    return responses[responses.length - 1];
  };
  return { send, responses };
}

function registered(id: string, element: unknown = mount(id)): RegisteredElement {
  return { id, label: id, type: "button", element } as unknown as RegisteredElement;
}

function mount(id: string): HTMLElement {
  const el = document.createElement("button");
  el.id = id;
  el.textContent = id;
  document.body.appendChild(el);
  return el;
}

const rootStyle = () => document.documentElement.style;

// jsdom implements no pseudo-element styles; read the element's own instead,
// so `::before` / `::after` reads do not log "Not implemented" on every call.
const realGetComputedStyle = window.getComputedStyle.bind(window);

beforeEach(() => {
  vi.spyOn(window, "getComputedStyle").mockImplementation((el) => realGetComputedStyle(el));
  vi.mocked(responsiveSnapshotsOf).mockClear();
  vi.mocked(designAuditOf).mockClear();
  vi.mocked(designSnapshotOf).mockClear();
  vi.mocked(evaluateRegisteredQuality).mockClear();
});

afterEach(() => {
  vi.mocked(window.getComputedStyle).mockRestore();
  document.body.innerHTML = "";
  document.documentElement.removeAttribute("style");
});

describe("useDesignEvents — request shape", () => {
  it("a responsive {viewports:[480]} reaches the shared runtime and yields one snapshot at 480", async () => {
    const { send } = setup([registered("save")]);

    const response = await send("design_get_responsive", { viewports: [480] });

    expect(responsiveSnapshotsOf).toHaveBeenCalledTimes(1);
    const [elements, request] = vi.mocked(responsiveSnapshotsOf).mock.calls[0];
    expect(typeof elements).toBe("function");
    expect(request).toEqual({ viewports: [480] });
    expect(response).toMatchObject({ requestId: "req-1", success: true });
    const data = response.data as { snapshots: Array<{ viewportWidth: number }> };
    expect(data.snapshots).toHaveLength(1);
    expect(data.snapshots[0].viewportWidth).toBe(480);
    // The capture restores the page.
    expect(rootStyle().width).toBe("");
  });

  it("design_get_snapshot hands params to the runtime and reports the selection", async () => {
    const { send } = setup([registered("a"), registered("b")]);

    const response = await send("design_get_snapshot", { elementIds: ["b"] });

    expect(vi.mocked(designSnapshotOf).mock.calls[0][1]).toEqual({ elementIds: ["b"] });
    expect(response.success).toBe(true);
    const data = response.data as { elements: Array<{ elementId: string }>; returned: number };
    expect(data.elements.map((e) => e.elementId)).toEqual(["b"]);
    expect(data.returned).toBe(1);
  });

  it("set_viewport_constraints constrains and restores the root, measuring the width", async () => {
    const { send } = setup([]);

    const set = await send("set_viewport_constraints", { width: 480 });
    expect(set.success).toBe(true);
    expect(rootStyle().width).toBe("480px");
    expect(set.data).toMatchObject({ success: true, restored: false });

    const restored = await send("set_viewport_constraints", { restore: true });
    expect(restored.data).toMatchObject({ success: true, restored: true });
    expect(rootStyle().width).toBe("");
    expect(rootStyle().overflowX).toBe("");
  });
});

describe("useDesignEvents — typed failure codes", () => {
  it("an unknown element answers ELEMENT_NOT_FOUND", async () => {
    const { send } = setup([]);

    const response = await send("design_get_element_styles", { elementId: "ghost" });

    expect(response.success).toBe(false);
    expect(response.code).toBe("ELEMENT_NOT_FOUND");
    expect(response.error).toBeTruthy();
  });

  it("an element with no DOM node answers NO_DOM_REFERENCE", async () => {
    const { send } = setup([registered("detached", null)]);

    const response = await send("design_get_element_styles", { elementId: "detached" });

    expect(response).toMatchObject({ success: false, code: "NO_DOM_REFERENCE" });
  });

  it("a malformed design request answers INVALID_REQUEST (an empty viewport list)", async () => {
    const { send } = setup([registered("save")]);

    const response = await send("design_get_responsive", { viewports: [] });

    expect(response).toMatchObject({ success: false, code: "INVALID_REQUEST" });
  });

  it("a viewport constraint naming neither width nor restore answers INVALID_REQUEST", async () => {
    const { send } = setup([]);

    const response = await send("set_viewport_constraints", {});

    expect(response).toMatchObject({ success: false, code: "INVALID_REQUEST" });
    expect(rootStyle().width).toBe("");
  });

  it("a malformed quality request answers INVALID_REQUEST", async () => {
    const { send } = setup([registered("save")]);

    const response = await send("design_evaluate", { elementIds: "save" });

    expect(response).toMatchObject({ success: false, code: "INVALID_REQUEST" });
  });

  it("an unexpected runtime error answers the route's own code", async () => {
    vi.mocked(designSnapshotOf).mockImplementationOnce(() => {
      throw new Error("boom");
    });
    const { send } = setup([]);

    const response = await send("design_get_snapshot", {});

    expect(response).toMatchObject({
      success: false,
      error: "boom",
      code: "DESIGN_SNAPSHOT_ERROR",
    });
  });

  it("a diff with no saved baseline answers NO_BASELINE, then diffs once one is saved", async () => {
    const { send } = setup([registered("save")]);

    expect(await send("design_evaluate_diff", {})).toMatchObject({
      success: false,
      code: "NO_BASELINE",
    });

    const saved = await send("design_evaluate_baseline", { label: "before" });
    expect(saved).toMatchObject({ success: true, data: { saved: true, elementCount: 1 } });

    const diff = await send("design_evaluate_diff", {});
    expect(diff.success).toBe(true);
  });
});

describe("useDesignEvents — style guide fallback", () => {
  it("design_run_audit falls back to the loaded guide", async () => {
    const { send } = setup([registered("save")], GUIDE);

    const response = await send("design_run_audit", {});

    expect(vi.mocked(designAuditOf).mock.calls[0][2]).toBe(GUIDE);
    expect((response.data as { mode: string }).mode).toBe("style-guide");
  });

  it("design_run_audit with no guide anywhere runs the heuristics", async () => {
    const { send } = setup([registered("save")], null);

    const response = await send("design_run_audit", {});

    expect(response.success).toBe(true);
    expect((response.data as { mode: string }).mode).toBe("heuristic");
  });

  it("design_evaluate falls back to the loaded guide, and a request guide wins", async () => {
    const { send } = setup([registered("save")], GUIDE);

    await send("design_evaluate", {});
    expect(vi.mocked(evaluateRegisteredQuality).mock.calls[0][2]).toBe(GUIDE);

    const requestGuide = { ...GUIDE, name: "request-guide" } as StyleGuideConfig;
    await send("design_evaluate", { guide: requestGuide });
    expect(vi.mocked(evaluateRegisteredQuality).mock.calls[1][2]).toBe(requestGuide);
  });
});
