// @vitest-environment jsdom
/**
 * `useSavedBackendStatus` through a tiny harness component (React 19
 * `createRoot` + `act`, as the existing jsdom tests do — no
 * @testing-library). Every `get_web_integration_status` read is a deferred the
 * test settles by hand, so issue order and settle order are separate inputs.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Handler = (e: { payload: unknown }) => void;
const listeners = new Map<string, Set<Handler>>();

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));
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

import {
  signInControl,
  useSavedBackendStatus,
  type SavedBackendPhase,
  type SavedBackendStatusState,
  type SignInControl,
} from "./useSavedBackendStatus";
import type { SavedBackendStatus } from "@/lib/savedBackendUrl";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const GOOD: SavedBackendStatus = {
  backendUrl: "https://backend.example.test",
  settingsFault: null,
  backendUrlMachineLocal: false,
  backendUrlRefused: false,
};
const OTHER: SavedBackendStatus = { ...GOOD, backendUrl: "https://other.example.test" };
const FAULTED: SavedBackendStatus = { ...GOOD, settingsFault: { kind: "unreadable" } };
const NO_VERDICT: SavedBackendStatus = { backendUrl: "https://backend.example.test" };

interface Deferred {
  resolve: (v: unknown) => void;
  reject: (e: unknown) => void;
}

let host: HTMLDivElement;
let root: Root;
/** One deferred per `get_web_integration_status` read, in issue order. */
let reads: Deferred[];
let state: SavedBackendStatusState<SavedBackendStatus>;
const onReadError = vi.fn();

function Harness() {
  const current = useSavedBackendStatus({ onReadError });
  // Captured after commit (render stays pure); every read of `state` follows an `act`.
  useEffect(() => {
    state = current;
  });
  return null;
}

beforeEach(() => {
  listeners.clear();
  reads = [];
  onReadError.mockReset();
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd !== "get_web_integration_status") return Promise.resolve(undefined);
    return new Promise((resolve, reject) => reads.push({ resolve, reject }));
  });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

async function mount() {
  await act(async () => {
    root.render(<Harness />);
  });
}

async function settle(i: number, outcome: { ok: unknown } | { err: unknown }) {
  await act(async () => {
    if ("ok" in outcome) reads[i].resolve(outcome.ok);
    else reads[i].reject(outcome.err);
  });
}

async function emitChanged() {
  await act(async () => {
    for (const h of listeners.get("web-integration-changed") ?? []) h({ payload: null });
  });
}

async function retry() {
  await act(async () => {
    state.retry();
  });
}

describe("useSavedBackendStatus", () => {
  it("starts loading, then known with the read as both lastStatus and decisionStatus", async () => {
    await mount();
    expect(state.phase).toBe("loading");
    expect(state.lastStatus).toBeNull();
    expect(state.decisionStatus).toBeNull();
    await settle(0, { ok: GOOD });
    expect(state.phase).toBe("known");
    expect(state.lastStatus).toEqual(GOOD);
    expect(state.decisionStatus).toEqual(GOOD);
    expect(state.reading).toBe(false);
  });

  it("'reading' stays true across a superseding web-integration-changed read until THAT read settles", async () => {
    await mount();
    await settle(0, { err: "down" });
    expect(state.phase).toBe("unknown");
    await retry(); // read 1 (Retry's)
    expect(state.reading).toBe(true);
    await emitChanged(); // read 2 supersedes read 1
    expect(reads).toHaveLength(3);
    await settle(1, { ok: GOOD }); // the Retry's own read settles first — and is dropped
    expect(state.reading).toBe(true);
    expect(state.phase).toBe("unknown");
    await settle(2, { ok: OTHER }); // the latest-issued read settles
    expect(state.reading).toBe(false);
    expect(state.phase).toBe("known");
    expect(state.decisionStatus).toEqual(OTHER);
  });

  it("'reading' clears when the latest-issued read fails, and phase stays unknown", async () => {
    await mount();
    await settle(0, { ok: FAULTED });
    await retry();
    expect(state.reading).toBe(true);
    await settle(1, { err: "down" });
    expect(state.reading).toBe(false);
    expect(state.phase).toBe("unknown");
  });

  it("retry() while reading issues no read", async () => {
    await mount();
    await settle(0, { err: "down" });
    await retry();
    expect(reads).toHaveLength(2);
    await retry();
    expect(reads).toHaveLength(2);
  });

  it("retry() while loading issues no read", async () => {
    await mount();
    expect(state.phase).toBe("loading");
    await retry();
    expect(reads).toHaveLength(1);
    expect(state.reading).toBe(false);
  });

  it("out-of-order settles keep the latest: a newer success then an older failure stays known", async () => {
    await mount();
    await settle(0, { ok: GOOD });
    await emitChanged(); // read 1
    await emitChanged(); // read 2
    await settle(2, { ok: OTHER });
    await settle(1, { err: "down" });
    expect(state.phase).toBe("known");
    expect(state.lastStatus).toEqual(OTHER);
    expect(onReadError).not.toHaveBeenCalled();
  });

  it("out-of-order settles keep the latest: a newer failure then an older success stays unknown", async () => {
    await mount();
    await settle(0, { ok: GOOD });
    await emitChanged();
    await emitChanged();
    await settle(2, { err: "down" });
    await settle(1, { ok: OTHER });
    expect(state.phase).toBe("unknown");
    expect(state.lastStatus).toEqual(GOOD);
  });

  it("a failed re-read keeps lastStatus while decisionStatus goes null, and reports the error once", async () => {
    await mount();
    await settle(0, { ok: GOOD });
    await emitChanged();
    await settle(1, { err: "ipc down" });
    expect(state.phase).toBe("unknown");
    expect(state.lastStatus).toEqual(GOOD);
    expect(state.decisionStatus).toBeNull();
    expect(onReadError).toHaveBeenCalledTimes(1);
    expect(onReadError).toHaveBeenCalledWith("ipc down");
  });

  it("a read answering with a settingsFault is unknown: lastStatus kept, decisionStatus null", async () => {
    await mount();
    await settle(0, { ok: FAULTED });
    expect(state.phase).toBe("unknown");
    expect(state.lastStatus).toEqual(FAULTED);
    expect(state.decisionStatus).toBeNull();
  });

  it("a read missing the verdict fields is known (the surface names the doubt; no re-read fixes it)", async () => {
    await mount();
    await settle(0, { ok: NO_VERDICT });
    expect(state.phase).toBe("known");
    expect(state.decisionStatus).toEqual(NO_VERDICT);
  });
});

describe("signInControl — every input combination", () => {
  // Written from the rule, not from the code: busy wins, then reading, then the phase.
  const EXPECTED: Array<[SavedBackendPhase, boolean, boolean, SignInControl]> = [
    ["loading", false, false, "wait"],
    ["loading", false, true, "wait"],
    ["loading", true, false, "reading"],
    ["loading", true, true, "wait"],
    ["unknown", false, false, "retry"],
    ["unknown", false, true, "wait"],
    ["unknown", true, false, "reading"],
    ["unknown", true, true, "wait"],
    ["known", false, false, "sign-in"],
    ["known", false, true, "wait"],
    ["known", true, false, "reading"],
    ["known", true, true, "wait"],
  ];

  it("covers 3 phases × reading × busy", () => {
    expect(EXPECTED).toHaveLength(12);
  });

  it.each(EXPECTED)("phase=%s reading=%s busy=%s → %s", (phase, reading, busy, expected) => {
    expect(signInControl(phase, reading, busy)).toBe(expected);
  });
});
