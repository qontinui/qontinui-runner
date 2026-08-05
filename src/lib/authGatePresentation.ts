/**
 * HOW each auth-gate surface renders — the P2 companion to `resolveAuthGate`
 * (which decides WHICH surface renders; its semantics are untouched).
 *
 * The failure mode this encodes against: the gate surfaces used to render as
 * early returns ABOVE `<TerminalPage>`, so ANY auth state change (including a
 * transient probe blip) unmounted the whole terminal tree. The restore guard
 * (`didInitPages`, a useRef inside `useTerminalInitialization`) died with the
 * tree, letting the next mount re-run restore and type `claude --resume` over
 * still-alive sessions. #849/#850 stopped the known transient flips; this
 * makes EVERY trigger structurally harmless:
 *
 *   - `"loading"` / `"login"` → rendered as a full-viewport OPAQUE overlay
 *     ABOVE the permanently-mounted app tree. The tree below is made `inert`
 *     while an overlay is up, so it is both visually and interactively
 *     unreachable — but its PTYs, scrollback, and useRef guards survive by
 *     construction.
 *   - `"tier-unknown"` → stays an early return. The NO-DOWNGRADE hold screen
 *     fires when the runner tier could not be read at all, so we cannot know
 *     which tier-derived surfaces the tree below is even entitled to render.
 *     Covering an unknowable tree with an overlay would leave a mounted shell
 *     whose contents were decided from a tier we do not have; holding replaces
 *     it outright, which is the behavior `resolveAuthGate` was written for.
 *   - `"app"` → no gate surface at all.
 *
 * `"wizard"` USED to be listed here as an early return, on the reasoning that
 * it "only renders pre-setup, when no terminal tree exists yet, so there is
 * nothing to keep mounted". That sentence was the reasoning error behind a P0:
 * the wizard's own `currentStep` IS worth keeping mounted. Choosing a tier
 * dispatches `runner-tier-changed` (a tier re-read → `"loading"`) and, on a
 * primary runner, flips `isTier2` (re-arming the auth probe → `authResolving`
 * → `"loading"` for seconds); each one replaced the wizard and remounted it at
 * step 0, so every click on the Tier step looked like it did nothing. The
 * wizard is no longer a gate surface at all — `App` mounts it from a stable
 * position on `setupCompleted === false` alone, and these overlays cover it
 * exactly as they cover the app tree.
 *
 * Pure + exported so the overlay-vs-early-return decision is unit-testable in
 * vitest's node environment (no jsdom render of the provider stack needed).
 */

import type { AuthGate } from "./authGate";

/** Gate surfaces that render as an opaque overlay above the mounted tree. */
export type AuthGateOverlay = "loading" | "login";

/**
 * Gate surfaces that replace the tree outright. Exactly one:
 * `"tier-unknown"` — the tier is unreadable, so no tier-derived tree may be
 * rendered underneath at all.
 */
export type AuthGateEarlyReturn = "tier-unknown";

export type AuthGatePresentation =
  /** Replace the whole tree (nothing worth keeping mounted underneath). */
  | { mode: "early-return"; surface: AuthGateEarlyReturn }
  /** Keep the tree mounted; cover it with a full-viewport opaque overlay. */
  | { mode: "overlay"; surface: AuthGateOverlay }
  /** Authenticated (or gate not applicable): the app renders uncovered. */
  | { mode: "none" };

export function resolveAuthGatePresentation(gate: AuthGate): AuthGatePresentation {
  switch (gate) {
    case "tier-unknown":
      return { mode: "early-return", surface: "tier-unknown" };
    case "loading":
      return { mode: "overlay", surface: "loading" };
    case "login":
      return { mode: "overlay", surface: "login" };
    case "app":
      return { mode: "none" };
  }
}
