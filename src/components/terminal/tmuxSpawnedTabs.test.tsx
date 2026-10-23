/**
 * Every OPERATOR-initiated creation path shows its new session in tmux work
 * mode (plan `2026-10-11-runner-terminal-tmux-work-mode` D3: tmux
 * `new-window`): the UI Bridge `create-terminal`, the session manager's
 * Resume, "Fix finding" and "Open doc". Each hands the new tab to
 * `useZoneLayout().showSpawnedTab`, which is a no-op in grid mode.
 *
 * The hooks are driven for real: `renderToStaticMarkup` runs the hook body
 * once (vitest is `environment: "node"`), and the captured callbacks are then
 * invoked outside render.
 */

import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn() }));
vi.mock("@tauri-apps/plugin-fs", () => ({ writeTextFile: vi.fn() }));
vi.mock("./terminalHotStore", () => ({
  getTerminalHotStore: () => ({ getLastOutputLines: () => [] }),
}));
vi.mock("./approveAll", () => ({ writeToPaneOrReport: vi.fn(() => Promise.resolve()) }));

import { createShownTerminal } from "./terminalWorkModeControl";
import { useFindingsActions } from "./useFindingsActions";
import { useShellIntegration } from "./useShellIntegration";
import { useZoneActions } from "./useZoneActions";
import { resolveLayout } from "./useZoneLayout";
import type { TerminalWorkMode } from "./terminalWorkMode";
import type { TranscriptSession } from "./useTranscriptSessions";
import type { Finding } from "@/types/findings";

/** Run a hook once and hand back what it returned. */
function runHook<T>(hook: () => T): T {
  let out: T | null = null;
  function Harness() {
    out = hook();
    return null;
  }
  renderToStaticMarkup(<Harness />);
  return out!;
}

const noop = () => {};

describe("UI Bridge create-terminal → createShownTerminal", () => {
  it("shows the tab it created", async () => {
    const show = vi.fn();
    expect(await createShownTerminal(async () => "t-new", show)).toBe("t-new");
    expect(show.mock.calls).toEqual([["t-new"]]);
  });

  it("shows nothing when the backend created nothing", async () => {
    const show = vi.fn();
    expect(await createShownTerminal(async () => null, show)).toBeNull();
    expect(show).not.toHaveBeenCalled();
  });
});

describe("session manager Resume", () => {
  it("hands the resumed session's new tab to showSpawnedTab", async () => {
    const onResumedTab = vi.fn();
    const shell = runHook(() =>
      useShellIntegration({
        tabs: [],
        updateTab: noop,
        renameTab: noop,
        createTerminal: async () => "t-resumed",
        setSessionStates: noop,
        terminalRefs: { current: new Map() },
        setRightPanelMode: noop,
        setSelectedTranscriptSessionId: noop,
        pageId: "page-1",
        onResumedTab,
      }),
    );
    // Resume reads navigator.platform to pick the shell syntax. Node 20 (CI)
    // has no global navigator, so pin one rather than inherit the host's.
    vi.stubGlobal("navigator", { platform: "Linux x86_64" } as Navigator);
    vi.useFakeTimers();
    try {
      await shell.handleResumeSession({
        session_id: "0123456789abcdef",
        project_path: "/repo",
        config_dir: "/home/u/.claude",
      } as TranscriptSession);
    } finally {
      vi.useRealTimers();
      vi.unstubAllGlobals();
    }
    expect(onResumedTab.mock.calls).toEqual([["t-resumed"]]);
  });
});

describe("Fix finding", () => {
  it("hands the fix session's new tab to showSpawnedTab", async () => {
    const showSpawnedTab = vi.fn();
    const actions = runHook(() =>
      useFindingsActions({
        activeId: null,
        tabs: [],
        terminalRefs: { current: new Map() },
        createTerminal: async () => "t-fix",
        pendingResumeRef: { current: null },
        runGeneration: async () => {},
        setRightPanelMode: noop,
        showSpawnedTab,
      }),
    );
    vi.useFakeTimers();
    await actions.handleFixFinding({
      title: "broken thing",
      description: "it broke",
    } as Finding);
    vi.useRealTimers();
    expect(showSpawnedTab.mock.calls).toEqual([["t-fix"]]);
  });
});

describe("Open doc", () => {
  function openDoc(workMode: TerminalWorkMode, assignments: Record<number, string>) {
    const showSpawnedTab = vi.fn();
    const assignTabToZone = vi.fn();
    const layout = resolveLayout(workMode === "tmux" ? "single" : "quad", 4);
    const actions = runHook(() =>
      useZoneActions({
        pageId: "page-1",
        tabs: [],
        dispatch: noop,
        zoneLayout: {
          layoutId: layout.id,
          assignments,
          layout,
          setFocusedZone: noop,
          assignTabToZone,
          setLayoutId: noop,
          showSpawnedTab,
          workMode,
          isMultiZone: layout.zones.length > 1,
          toggleMaximize: noop,
        },
        stateTracking: { sessionStates: {} },
        labelsAndTags: { zoneLabels: {}, setZoneLabel: noop },
        transitionEffects: { setUnseenNeedsInput: noop },
        createTerminal: async () => null,
        createPlanTab: () => "plan-1",
        incrementMetric: noop,
        setNotification: noop,
      }),
    );
    actions.handleOpenDocFile("/plans/p.md");
    return { showSpawnedTab, assignTabToZone };
  }

  it("tmux: the opened doc is shown in the one viewport (there is no empty zone to find)", () => {
    const { showSpawnedTab, assignTabToZone } = openDoc("tmux", { 0: "a" });
    expect(showSpawnedTab.mock.calls).toEqual([["plan-1"]]);
    expect(assignTabToZone).not.toHaveBeenCalled();
  });

  it("grid: the opened doc takes the first empty zone, as before", () => {
    const { showSpawnedTab, assignTabToZone } = openDoc("grid", { 0: "a", 1: "b" });
    expect(assignTabToZone.mock.calls).toEqual([[2, "plan-1"]]);
    expect(showSpawnedTab).not.toHaveBeenCalled();
  });
});
