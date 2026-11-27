// @vitest-environment jsdom
/**
 * The hook as mounted: `useAccountMigrationNotifications` is rendered (React
 * `createRoot` + `act`) with the Tauri event bus mocked, and the backend's
 * migration events are fired as the runner emits them, so the listener wiring
 * — not only the pure toast builders — is under test.
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

import { useAccountMigrationNotifications } from "./useAccountMigrationNotifications";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function emit(event: string, payload: unknown) {
  for (const h of listeners.get(event) ?? []) h({ payload });
}

let host: HTMLDivElement;
let root: Root;
const showToast = vi.fn();

function Harness() {
  useAccountMigrationNotifications(showToast);
  return null;
}

beforeEach(async () => {
  listeners.clear();
  showToast.mockReset();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () => {
    root.render(createElement(Harness));
  });
});

afterEach(async () => {
  await act(async () => {
    root.unmount();
  });
  host.remove();
});

describe("useAccountMigrationNotifications", () => {
  it("shows the held toast when the backend emits session-account-migration-held", async () => {
    await act(async () => {
      emit("session-account-migration-held", {
        claudeSessionId: "0c4e3025-8c5c-4792-a3ca-201ad43e0113",
        fromConfigDir: "/home/u/.claude-sales",
        toConfigDir: "/home/u/.claude-support",
        reason: "claude_cli_unavailable:absent",
        ceilingSecs: 900,
      });
    });
    expect(showToast).toHaveBeenCalledTimes(1);
    expect(showToast).toHaveBeenCalledWith(
      "Session 0c4e3025 is out of tokens on .claude-sales; moving it to .claude-support is waiting for the Claude CLI to come back (up to 15 min)",
      "info",
    );
  });

  it("ends a held migration with the abandoned toast", async () => {
    await act(async () => {
      emit("session-account-migration-abandoned", {
        claudeSessionId: "0c4e3025-8c5c-4792-a3ca-201ad43e0113",
        fromConfigDir: "/home/u/.claude-sales",
        toConfigDir: "/home/u/.claude-support",
      });
    });
    expect(showToast).toHaveBeenCalledWith(
      "Session 0c4e3025 was not moved from .claude-sales to .claude-support: the old pane closed or exited during the migration",
      "info",
    );
  });

  it("closes a held operator move with the failed toast", async () => {
    await act(async () => {
      emit("session-account-migration-held", {
        claudeSessionId: "0c4e3025-8c5c-4792-a3ca-201ad43e0113",
        fromConfigDir: "/home/u/.claude-sales",
        toConfigDir: "/home/u/.claude-support",
        ceilingSecs: 900,
        trigger: "operator",
      });
      emit("session-account-migration-failed", {
        claudeSessionId: "0c4e3025-8c5c-4792-a3ca-201ad43e0113",
        fromConfigDir: "/home/u/.claude-sales",
        toConfigDir: "/home/u/.claude-support",
        reason: "exec failed",
      });
    });
    expect(showToast.mock.calls).toEqual([
      [
        "Moving session 0c4e3025 from .claude-sales to .claude-support is waiting for the Claude CLI to come back (up to 15 min)",
        "info",
      ],
      [
        "Moving session 0c4e3025 from .claude-sales to .claude-support failed: exec failed",
        "error",
      ],
    ]);
  });

  it("stops listening once unmounted", async () => {
    const events = [
      "session-account-migration-held",
      "session-account-migrated",
      "session-account-migration-skipped",
      "session-account-migration-abandoned",
      "session-account-migration-failed",
      "session-limit-parked",
      "session-limit-reset-continued",
      "session-limit-reset-continue-gave-up",
    ];
    // Each name is one the hook really listens to (review r8 A1).
    for (const event of events) {
      expect(listeners.get(event)?.size, event).toBe(1);
    }
    await act(async () => {
      root.unmount();
    });
    // Every listener is gone, not only the held one (review r7 A6).
    for (const event of events) {
      expect(listeners.get(event)?.size, event).toBe(0);
      emit(event, { ceilingSecs: 60 });
    }
    expect(showToast).not.toHaveBeenCalled();
    // Re-mount so afterEach's unmount has a live root.
    root = createRoot(host);
    await act(async () => {
      root.render(createElement(Harness));
    });
  });
});
