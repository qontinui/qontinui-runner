// @vitest-environment jsdom
/**
 * Mounting `useGlobalLogSources` reads the log source SETTINGS and nothing
 * else — no log content.
 *
 * Plan `2026-11-11-runner-glibc-arena-retention-and-whole-log-tail` Phase 2:
 * the hook used to call `read_global_log_sources` whenever its settings
 * loaded, so every webview load (`App.tsx`) and every `SettingsPanel` mount
 * made the runner read its whole day log — 2.9 GB measured — into memory on
 * the main thread, to return 100 lines no consumer read. Both consumers take
 * only `settings`.
 *
 * The settings below hold an enabled source with `include_all_when_no_profile`
 * set: exactly the shape that made the old hook issue the log read. And the
 * test waits for the settings to land, so a log read keyed on them would have
 * fired before the assertion.
 *
 * The runner's vitest config is `environment: "node"` and the repo carries no
 * React Testing Library, so this file opts into jsdom (line 1) and mounts the
 * hook with `createRoot` + `act`.
 */

import { act, createElement, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { useGlobalLogSources, type UseGlobalLogSourcesReturn } from "./useGlobalLogSources";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const SETTINGS = {
  sources: [
    {
      id: "source-runner",
      name: "Runner",
      description: "The runner's own day log",
      category: "runner",
      type: "file",
      path: "/var/log/qontinui-runner.log",
      tail_lines: 100,
      enabled: true,
      keywords: [],
      format: "plaintext",
      parser: "generic",
      timezone: "local",
      error_patterns: [],
      warning_patterns: [],
      ignore_patterns: [],
      poll_interval_ms: 5000,
    },
  ],
  profiles: [],
  ai_selection_mode: "dynamic",
  include_all_when_no_profile: true,
};

function invokedCommands(): string[] {
  return vi.mocked(invoke).mock.calls.map((call) => String(call[0]));
}

/** Let the settings read resolve and every effect it triggers run. */
async function settle(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 5; i += 1) {
      await Promise.resolve();
    }
  });
}

let container: HTMLDivElement;
let root: Root;
/** The latest committed hook result, published by `Probe` after each commit. */
const out: { current: UseGlobalLogSourcesReturn | null } = { current: null };

function Probe() {
  const result = useGlobalLogSources();
  useLayoutEffect(() => {
    out.current = result;
  });
  return null;
}

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  vi.mocked(invoke).mockReset();
  // A fresh settings object per read, so a reload changes `settings`' identity
  // and re-runs anything keyed on it — as a real IPC round-trip does.
  vi.mocked(invoke).mockImplementation(async (cmd: string) =>
    cmd === "get_global_log_sources"
      ? { success: true, data: structuredClone(SETTINGS) }
      : { success: true, data: [] },
  );
  out.current = null;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => {
    root.unmount();
  });
  container.remove();
});

describe("useGlobalLogSources", () => {
  it("mounting issues only get_global_log_sources", async () => {
    act(() => {
      root.render(createElement(Probe));
    });
    await settle();

    // The settings landed — so an effect keyed on them has already run.
    expect(out.current?.settings).toEqual(SETTINGS);
    expect(out.current?.loading).toBe(false);
    expect(invokedCommands()).toEqual(["get_global_log_sources"]);
  });

  it("reloading the settings reads no log content either", async () => {
    act(() => {
      root.render(createElement(Probe));
    });
    await settle();

    await act(async () => {
      await out.current?.loadSettings();
    });
    await settle();

    expect(invokedCommands()).toEqual(["get_global_log_sources", "get_global_log_sources"]);
  });
});
