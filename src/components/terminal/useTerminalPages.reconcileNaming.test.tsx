// @vitest-environment jsdom
/**
 * The seam the 2026-11-05 duplicates entered by (plan 2026-11-05 Phase 5):
 * the mounted hook's reconcile reads the backend's page ids and mints a tab
 * for each one it does not know. Over the measured pre-restart roster, the
 * old count rule named the restored batch "Page 19" … "Page 27" beside the
 * October "Page 20" … "Page 23".
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import { instanceStorage } from "@/lib/instance-storage";
import { RESTART_BATCH_PAGE_IDS, ROSTER_BEFORE_RESTART } from "./fixtures/terminalPageRoster";
import { PAGES_STORAGE_KEY, useTerminalPages, type TerminalPageConfig } from "./useTerminalPages";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  localStorage.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  localStorage.clear();
  invokeMock.mockReset();
});

describe("the mounted hook's reconcile over the 2026-11-05 roster", () => {
  it("mints the restart batch's tabs without taking a name already present", async () => {
    instanceStorage.setJSON(PAGES_STORAGE_KEY, ROSTER_BEFORE_RESTART);
    // The restart restored rows onto nine page ids the roster does not know.
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_list") {
        return {
          success: true,
          data: { terminals: RESTART_BATCH_PAGE_IDS.map((pageId, i) => ({ id: `t${i}`, pageId })) },
        };
      }
      if (cmd === "terminal_session_list_open") return { data: { sessions: [] } };
      throw new Error(`unexpected ${cmd}`);
    });

    function Probe() {
      useTerminalPages();
      return null;
    }
    await act(async () => {
      root.render(createElement(Probe));
    });

    const roster = instanceStorage.getJSON<TerminalPageConfig[]>(PAGES_STORAGE_KEY, []);
    const minted = roster.slice(ROSTER_BEFORE_RESTART.length);
    expect(minted.map((p) => p.id)).toEqual([...RESTART_BATCH_PAGE_IDS]);
    expect(minted.map((p) => p.name)).toEqual([
      "Page 24",
      "Page 25",
      "Page 26",
      "Page 27",
      "Page 28",
      "Page 29",
      "Page 30",
      "Page 31",
      "Page 32",
    ]);
    expect(minted.map((p) => p.origin)).toEqual(Array(9).fill("auto"));
    const names = roster.map((p) => p.name);
    const legacy = new Set(ROSTER_BEFORE_RESTART.map((p) => p.name));
    for (const p of minted) {
      expect(legacy.has(p.name)).toBe(false);
      expect(names.filter((n) => n === p.name)).toHaveLength(1);
    }
  });
});
