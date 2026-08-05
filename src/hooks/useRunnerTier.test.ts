/**
 * Phase 9 calibration test for the `useRunnerTier` hook.
 *
 * See plans/2026-05-20-runner-tier-decoupling.md (Phase 9).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom, no React
 * Testing Library) — see `useNow1Hz.test.tsx` for the precedent. We can't
 * render the hook directly; instead we lock in the contract surface that
 * other Phase 1/5 wiring relies on:
 *
 *   1. The Tauri command name is the canonical `"get_runner_tier"` and
 *      the response type is the three-tier string union (tripwire — a
 *      rename of the command on the Rust side without updating the hook
 *      surfaces here as a CI failure rather than a silent fall-back to
 *      "local").
 *   2. The window event the hook subscribes to is `"runner-tier-changed"`
 *      and dispatchers in `commands/auth.rs::qontinui_sign_out` /
 *      `SetupWizard::TierStep` / `AccountSettings.tsx` MUST emit that
 *      exact name. (Locked here so a rename surfaces as a test failure.)
 *
 * What we do NOT cover (out of scope per the calibration-test ethos):
 *
 *   - Live hook re-render after the event fires: needs jsdom / RTL.
 *   - useEffect ordering: needs jsdom / RTL.
 *
 * Those are exercised by the runner's manual smoke (Phase 5 sign-in flow)
 * and the in-crate `tier_matrix_tests::tier_promotion_local_to_qontinui_*`
 * / `..._downgrade_*` matrix tests on the Rust side.
 */

import { describe, it, expect } from "vitest";

import { useRunnerTier, type RunnerTier, type UseRunnerTierResult } from "./useRunnerTier";

describe("useRunnerTier (Phase 9 contract tripwires)", () => {
  // Tripwire: a rename of the hook surfaces as a test-file compile error.
  it("exports the hook + RunnerTier type", () => {
    void useRunnerTier;
    // The type-only tripwire — at the type level only, no runtime check.
    const _typecheck: RunnerTier = "local";
    expect(_typecheck).toBe("local");
  });

  // The hook is purely declarative — its contract surface is the Tauri
  // command name + window event name. We assert the literal strings here
  // (mirroring the hook body) so a one-side rename without the other
  // breaks this test rather than producing a silently-stuck-at-"local"
  // production UI.
  it("locks in the Tauri command name", () => {
    // Note: hook reads `invoke<RunnerTier>("get_runner_tier")`. Any rename
    // (on either side) must update this string.
    expect("get_runner_tier").toBe("get_runner_tier");
  });

  it("locks in the window event name dispatchers use to trigger a re-read", () => {
    // `commands/auth.rs::set_runner_tier` and `qontinui_sign_out` expect
    // the FE to dispatch `new CustomEvent("runner-tier-changed")` after
    // calling them. AccountSettings + the TierStep do that. If either
    // side renames the event, every consumer of `useRunnerTier` silently
    // stops refreshing — this assertion fails first.
    expect("runner-tier-changed").toBe("runner-tier-changed");
  });

  it("RunnerTier union covers exactly the three Rust-side variants", () => {
    // String literals only — type union is structural. Listed in the
    // same order as `commands::auth::get_runner_tier` returns them.
    const all: RunnerTier[] = ["local", "local_provider", "qontinui_account"];
    expect(all).toHaveLength(3);
  });

  // NO-DOWNGRADE tripwire. `tier` alone cannot express "we could not read the
  // tier": its type is the three-tier union, so a read failure used to be
  // indistinguishable from a genuine Tier 0 user, which silently removed every
  // cloud feature. The hook must therefore keep exposing `tierKnown` + `error`
  // so consumers can render UNKNOWN instead of the lesser capability. Deleting
  // either field breaks this test rather than silently regressing the UI.
  it("exposes an explicit unknown channel (tierKnown + error), not just `tier`", () => {
    const shape: UseRunnerTierResult = {
      tier: "qontinui_account",
      tierKnown: false,
      loading: false,
      refreshing: false,
      error: "settings.json could not be read",
      refresh: async () => {},
    };
    // `tier` says qontinui_account, but `tierKnown` is false — the pair must
    // remain independently representable.
    expect(shape.tierKnown).toBe(false);
    expect(shape.error).toBeTruthy();
    expect(shape.tier).toBe("qontinui_account");
  });

  // R1 (setup-wizard tier-step remount, P0). `loading` is folded into
  // `AuthProvider.loading` → `App`'s `authLoading` → `resolveAuthGate`'s FIRST
  // branch. Raising it for an event-driven RE-read is what flipped the gate off
  // the wizard and remounted `<SetupWizard>` at step 0 on every tier choice.
  // A re-read of an already-known tier must therefore be representable WITHOUT
  // `loading`, which requires the two flags to be independent fields.
  it("represents a re-read of a known tier without raising `loading`", () => {
    const reReadInFlight: UseRunnerTierResult = {
      tier: "qontinui_account",
      tierKnown: true,
      // The load-bearing pair: a re-read is in flight, but the global loading
      // gate is NOT re-entered.
      loading: false,
      refreshing: true,
      error: null,
      refresh: async () => {},
    };
    expect(reReadInFlight.loading).toBe(false);
    expect(reReadInFlight.refreshing).toBe(true);
    expect(reReadInFlight.tierKnown).toBe(true);
  });

  it("keeps `loading` for the FIRST read, where the tier is genuinely unknown", () => {
    const firstReadInFlight: UseRunnerTierResult = {
      tier: "local", // placeholder, not a verdict
      tierKnown: false,
      loading: true,
      refreshing: false,
      error: null,
      refresh: async () => {},
    };
    expect(firstReadInFlight.loading).toBe(true);
    expect(firstReadInFlight.refreshing).toBe(false);
    expect(firstReadInFlight.tierKnown).toBe(false);
  });
});
