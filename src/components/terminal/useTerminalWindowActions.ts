import { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";
import { setPageBinding } from "@/lib/pageBindings";
import { zoneLayoutStorageKey } from "./useZoneLayout";

/**
 * A runner-owned OS window (the main window or a `term-N` pop-out), as
 * returned by the `list_runner_windows` Tauri command. Mirrors the Rust
 * `WindowRecord` — only the fields the terminal UI needs are typed here.
 */
export interface RunnerWindowRecord {
  label: string;
  kind: "main" | "pop_out";
}

/**
 * Per-terminal window operations shared by the zone hover toolbar
 * ("Send to window" menu) and the drag-tab-out gesture. Thin wrappers over
 * the same Tauri commands the `useUIComponent` actions in `TerminalPage`
 * already use, so the human-facing and agent-facing surfaces stay in lockstep.
 */
export function useTerminalWindowActions() {
  /** Open a fresh pop-out window and move `tabId` into it. Returns the new
   *  window's label (e.g. `"term-2"`). */
  const popOutTab = useCallback(async (tabId: string): Promise<string> => {
    const rec = await invoke<{ label: string }>("open_terminal_window", { placement: null });
    await invoke("assign_session_to_window", { sessionId: tabId, windowLabel: rec.label });
    return rec.label;
  }, []);

  /** Move `tabId` to an existing window (`"main"` or `"term-N"`). */
  const moveTabToWindow = useCallback(
    async (tabId: string, windowLabel: string): Promise<void> => {
      await invoke("assign_session_to_window", { sessionId: tabId, windowLabel });
    },
    [],
  );

  /** Enumerate the runner's own windows (main + pop-outs). */
  const listWindows = useCallback(async (): Promise<RunnerWindowRecord[]> => {
    return await invoke<RunnerWindowRecord[]>("list_runner_windows");
  }, []);

  /**
   * Detach an ENTIRE page into its own pop-out window (the "pop out page"
   * feature). Opens a window BOUND to `pageId` (so its terminals are claimed by
   * page, survive restarts, and vanish from the main window), copies the page's
   * zone layout so the grid looks identical, and records the binding in the
   * synchronous mirror so the main window hides the page immediately. Returns
   * the new window's label.
   *
   * No per-terminal `assign_session_to_window` calls are needed: page-binding
   * (`WindowRecord.bound_page`) is what routes every one of the page's tabs to
   * the new window.
   */
  const popOutPage = useCallback(async (pageId: string): Promise<string> => {
    const rec = await invoke<{ label: string }>("open_terminal_window", {
      placement: null,
      boundPage: pageId,
    });
    // Preserve the visual grid: clone the page's main-window layout into the
    // pop-out's per-(page, window) key BEFORE its React app reads it on boot.
    try {
      const srcKey = zoneLayoutStorageKey(pageId, "main");
      const layout = instanceStorage.getJSON<unknown>(srcKey, null);
      if (layout !== null) {
        instanceStorage.setJSON(zoneLayoutStorageKey(pageId, rec.label), layout);
      }
    } catch {
      // Layout copy is best-effort — the pop-out falls back to auto-layout.
    }
    // Optimistic mirror update so the main window hides + switches off the page
    // without waiting for the Rust snapshot refresh (window-opened) to arrive.
    setPageBinding(pageId, rec.label);
    return rec.label;
  }, []);

  return { popOutTab, moveTabToWindow, listWindows, popOutPage };
}
