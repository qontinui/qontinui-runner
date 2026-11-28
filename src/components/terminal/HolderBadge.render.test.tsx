// @vitest-environment jsdom
/**
 * The REAL `HolderBadge` against a mocked `terminal_holder_states` read: a
 * tab switched to another pane drops the last pane's state, a pane not yet
 * in the registry is re-read on ONE capped fast chain, and a listed pane
 * renders its marker.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const listing = vi.hoisted(() => ({ data: {} as Record<string, unknown> }));
const invoke = vi.hoisted(() => vi.fn(async () => ({ success: true, data: { ...listing.data } })));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { FIRST_READ_RETRIES, FIRST_READ_RETRY_MS, HolderBadge } from "./HolderBadge";

const unsafe = {
  holder: false,
  upgradeSafe: false,
  notUpgradeSafe: { reason: "holder_disabled" },
};

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockClear();
  listing.data = {};
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.useRealTimers();
});

async function flush(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

const badge = () => container.querySelector("[data-holder-state]");

describe("HolderBadge", () => {
  it("renders the marker for a listed pane that is not upgrade-safe", async () => {
    listing.data = { a: unsafe };
    act(() => root.render(createElement(HolderBadge, { terminalId: "a" })));
    await flush();
    expect(badge()?.getAttribute("data-holder-state")).toBe("holder_disabled");
  });

  it("drops the previous pane's state when the tab switches to an unlisted pane", async () => {
    listing.data = { a: unsafe };
    act(() => root.render(createElement(HolderBadge, { terminalId: "a" })));
    await flush();
    expect(badge()).not.toBeNull();
    act(() => root.render(createElement(HolderBadge, { terminalId: "b" })));
    await flush();
    expect(badge()).toBeNull();
  });

  it("re-reads an unlisted pane on one capped chain, not one per poll", async () => {
    act(() => root.render(createElement(HolderBadge, { terminalId: "late" })));
    await flush();
    // Long enough for several 30 s polls and every fast retry.
    await flush(5 * 60_000);
    // The mount read, the capped fast retries, and one read per 30 s poll.
    const polls = Math.floor((5 * 60_000) / 30_000);
    expect(invoke.mock.calls.length).toBe(1 + FIRST_READ_RETRIES + polls);
    expect(FIRST_READ_RETRIES * FIRST_READ_RETRY_MS).toBeLessThan(5 * 60_000);
  });

  it("shows a pane once the registry lists it", async () => {
    act(() => root.render(createElement(HolderBadge, { terminalId: "late" })));
    await flush();
    expect(badge()).toBeNull();
    listing.data = { late: unsafe };
    await flush(FIRST_READ_RETRY_MS);
    expect(badge()).not.toBeNull();
  });
});
