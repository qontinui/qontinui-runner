// @vitest-environment jsdom
/**
 * `createTerminal`, entered through the REAL `useTerminalManager` mounted in
 * React, the way the session restore enters it (plan
 * `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`
 * Phase 5): with a spawn source that has nobody to answer the guard's dialog
 * (`onRefusal: "defer"`). When the runner refuses the spawn at its resource
 * guard, the promise must settle at once, rejecting so the restore can retry the
 * record, and the operator dialog must never open. Left open on a headless box,
 * that dialog is where an unplanned-restart restore used to stop for good.
 */

import { act, createElement, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const refusal = vi.hoisted(() => ({ text: "" }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "terminal_create") throw refusal.text;
    if (cmd === "terminal_list") return { success: true, data: { terminals: [] } };
    return { success: true, data: null };
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { getSnapshot, resolvePendingResourceBlock, subscribe } from "@/lib/resourceGuard";
import wire from "@/lib/resourceGuardWire.fixture.json";
import { useTerminalManager } from "./useTerminalManager";

type CreateTerminal = (
  title?: string,
  workingDir?: string,
  tenantId?: string,
  source?: { label: string; onRefusal?: string },
) => Promise<string | null>;

const captured: { createTerminal: CreateTerminal | null } = { createTerminal: null };

function Harness() {
  const manager = useTerminalManager("default", "main", "/w");
  const createTerminal = manager.createTerminal as CreateTerminal;
  useEffect(() => {
    captured.createTerminal = createTerminal;
  }, [createTerminal]);
  return null;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  refusal.text = wire.refusals.thread_count;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  while (getSnapshot() !== null) resolvePendingResourceBlock(false);
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("useTerminalManager.createTerminal, entered by the session restore", () => {
  it("a restore-source spawn the guard refuses settles at once and never opens the dialog", async () => {
    await act(async () => {
      root.render(createElement(Harness));
    });
    let dialogOpened = false;
    const stop = subscribe(() => {
      if (getSnapshot() !== null) dialogOpened = true;
    });

    const outcome = captured.createTerminal!("restored", "/w/1", undefined, {
      label: "session restore",
      onRefusal: "defer",
    }).then(
      (id) => `resolved:${id}`,
      () => "rejected",
    );
    const settled = await Promise.race([
      outcome,
      new Promise<string>((resolve) => setTimeout(() => resolve("still waiting"), 200)),
    ]);
    stop();

    expect(dialogOpened).toBe(false);
    expect(getSnapshot()).toBeNull();
    // Rejected, not resolved to null: the restore must see the refusal to retry it.
    expect(settled).toBe("rejected");
  });
});
