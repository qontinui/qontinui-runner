/**
 * Shared types for the terminal component directory.
 */

/** Standard Tauri command response used by terminal hooks and components. */
export interface CommandResponse {
  success: boolean;
  message: string | null;
  data: unknown;
}
