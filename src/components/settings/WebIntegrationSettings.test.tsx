// @vitest-environment jsdom
/**
 * The Web Integration page can never spin forever (plan
 * 2026-10-11-runner-coord-credential-self-renewal-land-and-close-the-dark-gap,
 * G2 / Phase 5 step 2).
 *
 * With the webview IPC wedged, `invoke("get_web_integration_status")` returns
 * a promise that never settles. Before the bound, the page sat on "Loading…"
 * indefinitely and the pair-code control — the operator's recovery path —
 * never rendered. The REAL component is rendered (React 19 `createRoot` +
 * `act`, no Testing Library) with `invoke` mocked so the status command never
 * answers and every other command rejects.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/** When set, the NEXT status invoke resolves with this value (once). */
let nextStatusReply: unknown = null;

const STATUS_B = {
  enabled: true,
  backendUrl: "https://api.example.test",
  webBaseUrl: "",
  runnerTokenMasked: "",
  runnerId: "runner-b",
  lastHeartbeatAt: null,
  registrationError: null,
  wsConnected: true,
  settingsFault: null,
};

const invokeMock = vi.fn((command: string): Promise<unknown> => {
  if (command === "get_web_integration_status") {
    if (nextStatusReply !== null) {
      const reply = nextStatusReply;
      nextStatusReply = null;
      return Promise.resolve(reply);
    }
    // Never settles — the wedged-webview shape.
    return new Promise<unknown>(() => {});
  }
  return Promise.reject(new Error(`${command}: not mocked`));
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string) => invokeMock(command),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { WebIntegrationSettings } from "./WebIntegrationSettings";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function statusCalls(): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "get_web_integration_status").length;
}

describe("WebIntegrationSettings status load", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(async () => {
    vi.useFakeTimers();
    invokeMock.mockClear();
    nextStatusReply = null;
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    await act(async () => {
      root.render(createElement(WebIntegrationSettings, { onLog: () => {} }));
    });
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("is still loading just before the 10 s bound", async () => {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(9_999);
    });
    expect(container.textContent).toContain("Loading");
    expect(container.querySelector("#web-integration-pair-code")).toBeNull();
  });

  it("renders the error state, Retry and the pair-code input once the bound elapses", async () => {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(container.textContent).not.toContain("Loading");

    const banner = container.querySelector("#banner-web-integration-status-error");
    expect(banner).not.toBeNull();
    expect(banner?.textContent).toContain(
      "get_web_integration_status failed: did not answer within 10s",
    );
    // The command is named once, by the banner — not repeated inside the error.
    expect(banner?.textContent).not.toContain("get_web_integration_status did not answer");

    const retry = container.querySelector<HTMLButtonElement>("#web-integration-status-retry");
    expect(retry).not.toBeNull();
    expect(retry?.textContent).toBe("Retry");

    // The recovery control does not depend on the status load.
    expect(container.querySelector("#web-integration-pair-code")).not.toBeNull();
  });

  it("Retry re-invokes the status command", async () => {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    const before = statusCalls();

    const retry = container.querySelector<HTMLButtonElement>("#web-integration-status-retry");
    await act(async () => {
      retry?.click();
    });

    expect(statusCalls()).toBe(before + 1);
    expect(retry?.textContent).toBe("Retrying…");

    // The retry is bounded too: it settles back to an actionable Retry.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(
      container.querySelector<HTMLButtonElement>("#web-integration-status-retry")?.textContent,
    ).toBe("Retry");
  });

  it("an older load timing out cannot overwrite a newer success", async () => {
    // t=10 s: the initial load times out (banner up) and poll A starts; A
    // never settles.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(container.querySelector("#banner-web-integration-status-error")).not.toBeNull();

    // Retry B answers immediately with a connected status.
    nextStatusReply = STATUS_B;
    const retry = container.querySelector<HTMLButtonElement>("#web-integration-status-retry");
    await act(async () => {
      retry?.click();
    });
    expect(container.querySelector("#banner-web-integration-status-error")).toBeNull();
    expect(container.textContent).toContain("runner-b");

    // t=20 s: A's timeout fires AFTER B succeeded. It is stale and must not
    // bring the error banner back over B's status.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(container.querySelector("#banner-web-integration-status-error")).toBeNull();
    expect(container.textContent).toContain("runner-b");
  });
});
