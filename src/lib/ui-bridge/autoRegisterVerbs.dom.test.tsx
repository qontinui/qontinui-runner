// @vitest-environment jsdom
/**
 * What the runner's registry SERVES for an auto-registered text input, with the
 * SDK mounted the way `App.tsx` mounts it (`UIBridgeProvider` +
 * `AutoRegisterProvider {...RUNNER_AUTO_REGISTER_OPTIONS}`).
 *
 * Plan `2026-11-09-an-elements-advertised-verbs-depend-on-which-path-found-it`,
 * Phase 5. The runner's element-action door refuses, before IPC, any verb an
 * element does not advertise (`is_action_advertised`,
 * `src-tauri/src/mcp/ui_bridge/elements.rs`), and the advertised list it reads
 * is the `get_element` reply — `serializeElement` of the registry entry
 * (`hooks/ui-bridge-events/useControlEvents.ts`). Before `@qontinui/ui-bridge`
 * 0.46 the auto-register hook kept its own verb table, which gave an `<input>`
 * no `setValue` and no `sendKeys`, so `setValue` on an auto-registered input
 * answered `ACTION_NOT_SUPPORTED`. 0.46 reads every producer's verbs from one
 * table (`core/element-kind.ts`).
 *
 * The exact list asserted here is `autoRegisteredTextInput` in
 * `servedActions.0.46.json`, which the Rust test
 * `a_0_46_text_input_passes_the_pre_ipc_gate_for_set_value` (`elements.rs`)
 * reads with `include_str!` to prove the gate accepts `setValue` against it —
 * so the SDK serving a different list fails here, and editing the fixture to
 * follow it re-runs the Rust gate on the new list.
 */

import { act, createElement, useEffect, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AutoRegisterProvider,
  UIBridgeProvider,
  useUIBridge,
  type UIBridgeRegistry,
} from "@qontinui/ui-bridge";

import { isInteractiveElementRow } from "@/hooks/sdk-ui-bridge/utils";
import { serializeElement } from "@/hooks/ui-bridge-events/utils";
import { RUNNER_AUTO_REGISTER_OPTIONS } from "./autoRegisterOptions";
import servedActions from "./servedActions.0.46.json";

let container: HTMLDivElement;
let root: Root;
let registry: UIBridgeRegistry | null = null;

function CaptureRegistry() {
  const bridgeRegistry = useUIBridge().registry;
  useEffect(() => {
    registry = bridgeRegistry;
  }, [bridgeRegistry]);
  return null;
}

function mount(children: ReactNode) {
  act(() => {
    root.render(
      createElement(UIBridgeProvider, {
        features: {},
        children: createElement(AutoRegisterProvider, {
          ...RUNNER_AUTO_REGISTER_OPTIONS,
          children: [createElement(CaptureRegistry, { key: "probe" }), children],
        }),
      }),
    );
  });
}

/** The SDK's default media-registration debounce (`mediaDebounceMs`). */
const MEDIA_DEBOUNCE_MS = 200;

/** Wait out a scan debounce — by default the auto-register one (`debounceMs`). */
async function flushScan(debounceMs: number = RUNNER_AUTO_REGISTER_OPTIONS.debounceMs) {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, debounceMs * 3));
  });
}

beforeEach(() => {
  // jsdom lays nothing out, and the scanner skips a zero-size node as
  // invisible (`useAutoRegister` `isElementVisible`). Give every node a box.
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 10,
    y: 10,
    width: 120,
    height: 24,
    top: 10,
    left: 10,
    right: 130,
    bottom: 34,
    toJSON: () => ({}),
  } as DOMRect);
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  registry = null;
  vi.restoreAllMocks();
});

describe("an auto-registered text input, as the runner mounts the SDK", () => {
  it("serves the 0.46 text-input verbs, setValue and sendKeys among them", async () => {
    mount(createElement("input", { type: "text", "aria-label": "Email address" }));
    await flushScan();

    const input = container.querySelector("input");
    expect(input).not.toBeNull();
    const registered = registry?.getAllElements().find((el) => el.element === input);
    expect(registered, "the scanner registered the input").toBeDefined();
    expect(registered!.origin).toBe("auto");

    // What the runner's `get_element` IPC reply carries — the list the pre-IPC
    // gate reads.
    const served = serializeElement(registered!).actions;
    expect(served).toEqual(servedActions.autoRegisteredTextInput);
    for (const verb of ["setValue", "sendKeys", "type"]) expect(served).toContain(verb);
  });

  it("serves an auto-registered button the every-element verbs", async () => {
    // The Rust vocabulary reads this list to prove `press` on a 0.46 button
    // suggests `click`, not `sendKeys` (`action_vocabulary.rs`, D4).
    mount(createElement("button", { type: "button" }, "Save draft"));
    await flushScan();

    const button = container.querySelector("button");
    const registered = registry?.getAllElements().find((el) => el.element === button);
    expect(registered, "the scanner registered the button").toBeDefined();
    expect(serializeElement(registered!).actions).toEqual(servedActions.autoRegisteredButton);
  });

  it("serves a media row the every-element verbs, not an empty list", async () => {
    // Media discovery is on in the runner's mount (the SDK default; the
    // options do not turn it off), and `registerMediaElement` reads the table
    // too since 0.46. That is why the runner's affordance readers classify
    // media by category rather than by "has actions"
    // (`src-tauri/src/mcp/ui_bridge/element_kind.rs`,
    // `hooks/sdk-ui-bridge/utils.ts` `isInteractiveElementRow`).
    mount(createElement("canvas", { width: 120, height: 24, "aria-label": "Chart" }));
    await flushScan(MEDIA_DEBOUNCE_MS);

    const canvas = container.querySelector("canvas");
    const registered = registry?.getAllElements().find((el) => el.element === canvas);
    expect(registered, "the media scanner registered the canvas").toBeDefined();
    expect(registered!.category).toBe("media");
    expect(registered!.type).toBe("canvas");
    expect(registered!.actions).toContain("sendKeys");
    expect(registered!.actions).toContain("click");
    expect(isInteractiveElementRow(serializeElement(registered!))).toBe(false);
  });
});
