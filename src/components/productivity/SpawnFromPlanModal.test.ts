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

import {
  deriveSpawnFormState,
  spawnNoticeProps,
  SPAWN_SURFACE,
  type SpawnFormInput,
} from "./SpawnFromPlanModal";
import {
  coordDisabledCopy,
  runCoordDisabledAction,
} from "@/components/shared/CoordConnectionRequired";
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
  mode: "connected",
  base: "https://coord.qontinui.io",
  source: "env",
});
const isolatedNoAccount = deriveCoordGating({
  mode: "isolated",
  base: null,
  source: COORD_SOURCE_NO_ACCOUNT,
});
const isolatedUnreadable = deriveCoordGating({
  mode: "isolated",
  base: null,
  source: COORD_SOURCE_SETTINGS_UNREADABLE,
});
// `null` = the invoke rejected, or the first load is still in flight.
const unknown = deriveCoordGating(null);

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

describe("spawnNoticeProps — the modal's dismiss wiring (§6.4)", () => {
  // This helper exists ONLY so the wiring below is assertable under
  // `environment: "node"`, where the notice's button cannot be clicked.
  // Without these assertions the seam bought nothing.
  it("passes the modal's own close handler as the notice's onDismiss", () => {
    const onClose = () => {};
    const props = spawnNoticeProps(COORD_SOURCE_NO_ACCOUNT, onClose);
    // The modal is `fixed inset-0` with `aria-modal="true"`. If the notice's
    // action navigated without closing it, the tab switch would land BEHIND
    // an opaque overlay and the operator would see nothing happen.
    expect(props.onDismiss).toBe(onClose);
  });

  it("names the Spawn surface and its own bridge id", () => {
    const props = spawnNoticeProps(COORD_SOURCE_NO_ACCOUNT, () => {});
    expect(props.surface).toBe(SPAWN_SURFACE);
    expect(props.uiBridgeId).toBe("productivity.spawn-from-plan-modal-isolated");
  });

  it("forwards the source verbatim so the notice picks the right message", () => {
    // Both isolated arms reach this modal; collapsing them here would put the
    // "connect an account" copy in front of an operator whose settings.json
    // is the actual fault.
    expect(spawnNoticeProps(COORD_SOURCE_SETTINGS_UNREADABLE, () => {}).source).toBe(
      COORD_SOURCE_SETTINGS_UNREADABLE,
    );
    expect(spawnNoticeProps(null, () => {}).source).toBeNull();
  });

  it("wires the dismiss so the notice's action closes the modal first", () => {
    const calls: string[] = [];
    const props = spawnNoticeProps(COORD_SOURCE_NO_ACCOUNT, () => calls.push("close"));
    runCoordDisabledAction({
      copy: coordDisabledCopy(props.source, props.surface),
      onDismiss: props.onDismiss,
      dispatch: (tab) => calls.push(`dispatch:${tab}`),
    });
    expect(calls).toEqual(["close", "dispatch:settings-account"]);
  });
});
