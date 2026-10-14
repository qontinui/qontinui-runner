/**
 * Which account-selection mode the Settings page DESCRIBES.
 *
 * The runner reports the mode that actually governs
 * (`effective_account_selection_mode`) and why (`account_selection_source`).
 * Only a `fleet` source makes it differ from the local mode, so every other
 * source must describe the LOCAL value — which keeps the description in step
 * with an unsaved local edit — and ticking the pin hands it back to local too.
 */
import { describe, expect, it } from "vitest";

import { accountSelectionModeLabel, displayedAccountSelection, type AiSettings } from "../types";
import { DEFAULT_AI_SETTINGS } from "../ai-settings/types";

function settingsWith(patch: {
  local?: AiSettings["claude_cli"]["account_selection_mode"];
  pinned?: boolean;
  effective?: AiSettings["effective_account_selection_mode"];
  source?: AiSettings["account_selection_source"];
}): AiSettings {
  return {
    ...DEFAULT_AI_SETTINGS,
    claude_cli: {
      ...DEFAULT_AI_SETTINGS.claude_cli,
      account_selection_mode: patch.local,
      account_selection_pinned: patch.pinned,
    },
    effective_account_selection_mode: patch.effective,
    account_selection_source: patch.source,
  };
}

describe("displayedAccountSelection", () => {
  it("describes the fleet's mode when the fleet decided it", () => {
    expect(
      displayedAccountSelection(
        settingsWith({ local: "manual", effective: "least_usage", source: "fleet" }),
      ),
    ).toEqual({ mode: "least_usage", fleetManaged: true });
  });

  it("describes the local mode for a local or pinned source", () => {
    for (const source of ["local", "pinned"] as const) {
      expect(
        displayedAccountSelection(
          settingsWith({
            local: "manual",
            effective: "manual",
            source,
            pinned: source === "pinned",
          }),
        ),
      ).toEqual({ mode: "manual", fleetManaged: false });
    }
  });

  it("follows an unsaved local edit when the fleet did not decide", () => {
    // Loaded as local/manual, then the operator clicked Least Usage.
    expect(
      displayedAccountSelection(
        settingsWith({ local: "least_usage", effective: "manual", source: "local" }),
      ),
    ).toEqual({ mode: "least_usage", fleetManaged: false });
  });

  it("hands the description back to local once the pin is ticked", () => {
    expect(
      displayedAccountSelection(
        settingsWith({ local: "manual", pinned: true, effective: "least_usage", source: "fleet" }),
      ),
    ).toEqual({ mode: "manual", fleetManaged: false });
  });

  it("falls back to local against a runner that predates the view fields", () => {
    expect(displayedAccountSelection(settingsWith({ local: "highest_expected_usage" }))).toEqual({
      mode: "highest_expected_usage",
      fleetManaged: false,
    });
  });
});

describe("accountSelectionModeLabel", () => {
  it("labels every mode", () => {
    expect(accountSelectionModeLabel("manual")).toBe("Manual");
    expect(accountSelectionModeLabel("least_usage")).toBe("Least Usage");
    expect(accountSelectionModeLabel("highest_expected_usage")).toBe("Highest Expected Usage");
  });
});
