import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * The runner's window identity, kept free of React so a hook can read it
 * without loading a context module (`WindowAssignmentsContext` re-exports
 * both names).
 */
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
