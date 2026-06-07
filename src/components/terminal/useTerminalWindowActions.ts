import { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

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

  return { popOutTab, moveTabToWindow, listWindows };
}
