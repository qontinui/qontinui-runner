/**
 * Isolated-mode gating for the Spawn-from-Plan form — plan
 * `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration` §6.4.
 *
 * Spawning posts to coord's `POST /agents/spawn`. On an isolated runner that
 * endpoint does not exist, so the form must be genuinely inert rather than
 * accepting input it can never send.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom — see
 * FleetHealthPanel.test.tsx for the same constraint), so this exercises the
 * pure `deriveSpawnFormState` the JSX branches on.
 */

import { describe, expect, it } from "vitest";

import { deriveSpawnFormState, type SpawnFormInput } from "./SpawnFromPlanModal";
import {
  COORD_SOURCE_NO_ACCOUNT,
  COORD_SOURCE_SETTINGS_UNREADABLE,
  deriveCoordGating,
} from "@/contexts/CoordModeContext";

/** A fully-filled form — everything except the coord mode is valid. */
function filled(overrides: Partial<SpawnFormInput> = {}): SpawnFormInput {
  return {
    busy: false,
    workUnitSlug: "2026-08-18-runner-embedded-pg-parity-and-coord-http-migration",
    phase: "6.4",
    intent: "gate the coord-backed surfaces",
    initialPrompt: "You are the frontend half of §6.4.",
    selectedRepos: ["qontinui-runner"],
    otherRepos: "",
    ...overrides,
  };
}

const connected = deriveCoordGating({
  data: { mode: "connected", base: "https://coord.qontinui.io", source: "env" },
  error: null,
  loading: false,
});
const isolatedNoAccount = deriveCoordGating({
  data: { mode: "isolated", base: null, source: COORD_SOURCE_NO_ACCOUNT },
  error: null,
  loading: false,
});
const isolatedUnreadable = deriveCoordGating({
  data: { mode: "isolated", base: null, source: COORD_SOURCE_SETTINGS_UNREADABLE },
  error: null,
  loading: false,
});
const unknown = deriveCoordGating({
  data: null,
  error: "Command get_coord_mode not found",
  loading: false,
});

describe("deriveSpawnFormState — coord-mode gating (§6.4)", () => {
  it("connected → form live and submittable", () => {
    const s = deriveSpawnFormState(filled({ gating: connected }));
    expect(s.coordDisabled).toBe(false);
    expect(s.fieldsEnabled).toBe(true);
    expect(s.canSubmit).toBe(true);
  });

  it("isolated (no account) → every field and submit genuinely disabled", () => {
    const s = deriveSpawnFormState(filled({ gating: isolatedNoAccount }));
    expect(s.coordDisabled).toBe(true);
    expect(s.fieldsEnabled).toBe(false);
    // Even a perfectly valid form cannot be submitted — there is nothing
    // to submit it to.
    expect(s.canSubmit).toBe(false);
  });

  it("isolated (settings.json unreadable) → disabled the same way", () => {
    const s = deriveSpawnFormState(filled({ gating: isolatedUnreadable }));
    expect(s.coordDisabled).toBe(true);
    expect(s.fieldsEnabled).toBe(false);
    expect(s.canSubmit).toBe(false);
  });

  it("unknown mode → form STAYS enabled rather than being falsely disabled", () => {
    const s = deriveSpawnFormState(filled({ gating: unknown }));
    expect(s.coordDisabled).toBe(false);
    expect(s.fieldsEnabled).toBe(true);
    expect(s.canSubmit).toBe(true);
  });

  it("no gating supplied → unchanged pre-§6.4 behaviour (fails open)", () => {
    const s = deriveSpawnFormState(filled());
    expect(s.coordDisabled).toBe(false);
    expect(s.canSubmit).toBe(true);
  });

  it("still enforces the pre-existing field validation on a connected runner", () => {
    expect(deriveSpawnFormState(filled({ gating: connected, workUnitSlug: "  " })).canSubmit).toBe(
      false,
    );
    expect(deriveSpawnFormState(filled({ gating: connected, phase: "" })).canSubmit).toBe(false);
    expect(deriveSpawnFormState(filled({ gating: connected, intent: "" })).canSubmit).toBe(false);
    expect(deriveSpawnFormState(filled({ gating: connected, initialPrompt: "" })).canSubmit).toBe(
      false,
    );
    expect(
      deriveSpawnFormState(filled({ gating: connected, selectedRepos: [], otherRepos: "" }))
        .canSubmit,
    ).toBe(false);
    // Free-text repos alone are enough.
    expect(
      deriveSpawnFormState(
        filled({ gating: connected, selectedRepos: [], otherRepos: "some-repo" }),
      ).canSubmit,
    ).toBe(true);
  });

  it("an in-flight spawn disables the fields regardless of mode", () => {
    expect(deriveSpawnFormState(filled({ gating: connected, busy: true })).fieldsEnabled).toBe(
      false,
    );
  });
});
