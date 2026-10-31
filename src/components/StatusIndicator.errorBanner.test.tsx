// @vitest-environment jsdom
/**
 * The app error banner as the operator meets it: the REAL `StatusIndicator` is
 * rendered (React 19 `createRoot` + `act`, the `useSessionReview.test.ts`
 * pattern) with the Tauri event bus mocked, so a test fires the backend's
 * `error` / `error-cleared` events exactly as the runner emits them (plan
 * `2026-10-22-runner-terminal-overlays-cover-zone-content-and-controls` F1/F2).
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Handler = (e: { payload: unknown }) => void;
const listeners = new Map<string, Set<Handler>>();

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((event: string, handler: Handler) => {
    let set = listeners.get(event);
    if (!set) {
      set = new Set();
      listeners.set(event, set);
    }
    set.add(handler);
    return Promise.resolve(() => {
      set.delete(handler);
    });
  }),
}));

// The doctor badge polls the backend; it is not what these tests are about.
vi.mock("./doctor", () => ({ DoctorHealthBadge: () => null }));

import StatusIndicator from "./StatusIndicator";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function emit(event: string, payload: unknown) {
  for (const h of listeners.get(event) ?? []) h({ payload });
}

const NO_LEADER = {
  title: "Coord has no leader",
  message: "No coord replica is running the leader-gated plane.",
  error_code: "COORD_LIVENESS_NO_LEADER",
  severity: "warning",
  recoverable: true,
};
const INFO = {
  title: "Saved",
  message: "Settings saved.",
  error_code: "INFO_SAVED",
  severity: "info",
  recoverable: true,
};

let host: HTMLDivElement;
let root: Root;

async function mount() {
  await act(async () => {
    root.render(
      createElement(StatusIndicator, { pythonStatus: "running", executionActive: false }),
    );
  });
  // Let the `listen(...)` promises resolve and register.
  await act(async () => {});
}

const banner = () => host.querySelector('[data-ui-bridge-id="app-error-banner-COORD_LIVENESS_NO_LEADER"]');
const residual = () =>
  host.querySelector('[data-ui-bridge-id="app-error-residual-COORD_LIVENESS_NO_LEADER"]');
const dismiss = () =>
  host.querySelector<HTMLButtonElement>(
    '[data-ui-bridge-id="app-error-banner-dismiss-COORD_LIVENESS_NO_LEADER"]',
  );

beforeEach(() => {
  vi.useFakeTimers();
  listeners.clear();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.useRealTimers();
});

describe("StatusIndicator error banner", () => {
  it("an error event renders an in-flow banner with a named dismiss", async () => {
    await mount();
    await act(async () => emit("error", NO_LEADER));
    expect(banner()).not.toBeNull();
    expect(banner()!.className).not.toMatch(/\bfixed\b/);
    expect(dismiss()!.getAttribute("aria-label")).toBe("Dismiss: Coord has no leader");
  });

  it("dismiss collapses the fault to a residual instead of hiding it", async () => {
    await mount();
    await act(async () => emit("error", NO_LEADER));
    await act(async () => dismiss()!.click());
    expect(banner()).toBeNull();
    expect(residual()!.textContent).toContain("COORD_LIVENESS_NO_LEADER · last seen");
  });

  it("an error-cleared event for the code removes the residual", async () => {
    await mount();
    await act(async () => emit("error", NO_LEADER));
    await act(async () => dismiss()!.click());
    await act(async () => emit("error-cleared", { error_code: "COORD_LIVENESS_NO_LEADER" }));
    expect(residual()).toBeNull();
    expect(banner()).toBeNull();
  });

  it("an info that arrives and auto-hides never erases a dismissed live fault", async () => {
    await mount();
    await act(async () => emit("error", NO_LEADER));
    await act(async () => dismiss()!.click());
    await act(async () => emit("error", INFO));
    const info = () => host.querySelector('[data-ui-bridge-id="app-error-banner-info-INFO_SAVED"]');
    expect(info()).not.toBeNull();
    await act(async () => {
      vi.advanceTimersByTime(5000);
    });
    expect(info()).toBeNull();
    expect(residual()).not.toBeNull();
  });
});
