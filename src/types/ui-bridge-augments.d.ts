/**
 * Module augmentations for ui-bridge subpaths.
 *
 * These add exports that exist in the source but are missing from the built dist.
 * This file is a module (has top-level export) so augmentations apply correctly.
 */

export {};

// ---------------------------------------------------------------------------
// ui-bridge/ai — add parseNLAssertion and subscribeChanges on ChangeTrackerDeps
// ---------------------------------------------------------------------------

declare module "@qontinui/ui-bridge/ai" {
  export interface NLAssertionInput {
    target?: unknown;
    type?: string;
    expected?: unknown;
    assertion?: string;
  }

  export interface NLAssertionOutput {
    target: string;
    type: string;
    expected?: unknown;
  }

  export function parseNLAssertion(input: NLAssertionInput): NLAssertionOutput;

  // Augment ChangeTrackerDeps with subscribeChanges
  interface ChangeTrackerDeps {
    subscribeChanges?: (
      callback: (event: { type: string; timestamp: number }) => void,
    ) => () => void;
  }
}
