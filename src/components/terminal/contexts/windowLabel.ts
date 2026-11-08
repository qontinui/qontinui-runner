/**
 * This window's identity, with no React in it.
 *
 * Kept apart from `WindowAssignmentsContext` because that module calls
 * `createContext` at load time: a hook such as
 * `useTerminalPages` that only needs to know whether it runs in the main
 * window must not drag a React context into every importer's module graph.
 * A test that mocks `react` without `createContext` then fails to load at all
 * (`useShellIntegration.test.ts`, qontinui-runner#2750's first CI run).
 */
import { getCurrentWindow } from "@tauri-apps/api/window";

export const MAIN_WINDOW_LABEL = "main";

/** This window's Tauri label; `"main"` outside Tauri (tests / SSR). */
export function readWindowLabel(): string {
  try {
    return getCurrentWindow().label || MAIN_WINDOW_LABEL;
  } catch {
    // Non-Tauri env (tests / SSR) — behave as the single main window.
    return MAIN_WINDOW_LABEL;
  }
}
