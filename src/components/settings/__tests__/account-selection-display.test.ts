/**
 * Which account-selection mode the Settings page DESCRIBES.
 *
 * The runner reports the raw fleet-policy value (`fleet_account_selection_mode`)
 * beside its own resolution, and the page re-applies the runner's ONE rule
 * (`claude_accounts::resolve_selection`) to the CURRENT form state — so an
 * unsaved local mode or pin toggle is described correctly without a round
 * trip:
 *   pinned → local / pinned; unpinned & fleet value → fleet / fleet;
 *   otherwise → local / local.
 */
import { describe, expect, it } from "vitest";

import {
  accountSelectionModeLabel,
  displayedAccountSelection,
  type AccountSelectionMode,
  type AiSettings,
} from "../types";
import { DEFAULT_AI_SETTINGS } from "../ai-settings/types";

function settingsWith(patch: {
  local?: AccountSelectionMode;
  pinned?: boolean;
  fleet?: AccountSelectionMode | null;
  effective?: AccountSelectionMode;
  source?: AiSettings["account_selection_source"];
}): AiSettings {
  return {
    ...DEFAULT_AI_SETTINGS,
    claude_cli: {
      ...DEFAULT_AI_SETTINGS.claude_cli,
      account_selection_mode: patch.local,
      account_selection_pinned: patch.pinned,
    },
    fleet_account_selection_mode: patch.fleet,
    effective_account_selection_mode: patch.effective,
    account_selection_source: patch.source,
  };
}

describe("displayedAccountSelection — the runner's rule over form state", () => {
  const modes: AccountSelectionMode[] = ["manual", "least_usage", "highest_expected_usage"];
  const fleets: (AccountSelectionMode | null)[] = [null, ...modes];

  it("matches resolve_selection in every (local, pinned, fleet) cell", () => {
    let cells = 0;
    for (const local of modes) {
      for (const pinned of [true, false]) {
        for (const fleet of fleets) {
          const expected = pinned
            ? { mode: local, source: "pinned", fleetManaged: false }
            : fleet !== null
              ? { mode: fleet, source: "fleet", fleetManaged: true }
              : { mode: local, source: "local", fleetManaged: false };
          expect(displayedAccountSelection(settingsWith({ local, pinned, fleet }))).toEqual(
            expected,
          );
          cells += 1;
        }
      }
    }
    expect(cells).toBe(24);
  });

  it("un-ticking the pin while the fleet has a value shows the fleet's mode", () => {
    // Loaded pinned (runner said source=pinned), then the operator un-ticks.
    const loaded = settingsWith({
      local: "manual",
      pinned: true,
      fleet: "least_usage",
      effective: "manual",
      source: "pinned",
    });
    expect(displayedAccountSelection(loaded).source).toBe("pinned");
    const unticked = {
      ...loaded,
      claude_cli: { ...loaded.claude_cli, account_selection_pinned: false },
    };
    expect(displayedAccountSelection(unticked)).toEqual({
      mode: "least_usage",
      source: "fleet",
      fleetManaged: true,
    });
  });

  it("follows an unsaved local edit when the fleet has no opinion", () => {
    expect(
      displayedAccountSelection(
        settingsWith({ local: "least_usage", fleet: null, effective: "manual", source: "local" }),
      ),
    ).toEqual({ mode: "least_usage", source: "local", fleetManaged: false });
  });

  it("an unknown pin is treated as unpinned (the runner's default)", () => {
    expect(displayedAccountSelection(settingsWith({ local: "manual", fleet: "manual" }))).toEqual({
      mode: "manual",
      source: "fleet",
      fleetManaged: true,
    });
  });
});

describe("displayedAccountSelection — older runner without the raw fleet value", () => {
  it("uses the runner's own resolution when the fleet decided it", () => {
    expect(
      displayedAccountSelection(
        settingsWith({ local: "manual", effective: "least_usage", source: "fleet" }),
      ),
    ).toEqual({ mode: "least_usage", source: "fleet", fleetManaged: true });
  });

  it("hands the description back to local once the pin is ticked", () => {
    expect(
      displayedAccountSelection(
        settingsWith({ local: "manual", pinned: true, effective: "least_usage", source: "fleet" }),
      ),
    ).toEqual({ mode: "manual", source: "pinned", fleetManaged: false });
  });

  it("falls back to local when the runner reports no view fields at all", () => {
    expect(displayedAccountSelection(settingsWith({ local: "highest_expected_usage" }))).toEqual({
      mode: "highest_expected_usage",
      source: undefined,
      fleetManaged: false,
    });
  });
});

describe("accountSelectionModeLabel", () => {
  it("labels every mode", () => {
    expect(accountSelectionModeLabel("manual")).toBe("Manual");
    expect(accountSelectionModeLabel("least_usage")).toBe("Least Usage");
    expect(accountSelectionModeLabel("highest_expected_usage")).toBe("Highest Expected Usage");
    expect(accountSelectionModeLabel("weighted_random")).toBe("Weighted Random");
    expect(accountSelectionModeLabel("alternating_handoff")).toBe("Alternating Handoff");
  });
});
