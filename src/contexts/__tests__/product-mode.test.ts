/**
 * The visual-disclosure pin.
 *
 * `ProductModeContext` DERIVES the effective mode rather than storing it. That
 * is the whole design: the user's last mode is remembered, but only honoured
 * while the "visual" disclosure is on. Storing the effective mode instead would
 * mean a persisted `"visual"` could resurrect the visual sidebar for someone who
 * had since hidden it — or, if we reset the stored value on hide, that
 * re-enabling the disclosure would silently drop them back to AI Dev.
 */
import { describe, expect, it } from "vitest";

import { effectiveProductMode } from "../ProductModeContext";
import { DISCLOSURE_STORAGE_KEYS } from "../FeatureDisclosureContext";

describe("effectiveProductMode", () => {
  it("pins to AI Dev while the visual disclosure is off", () => {
    expect(effectiveProductMode("visual", false)).toBe("ai");
    expect(effectiveProductMode("ai", false)).toBe("ai");
  });

  it("honours the stored preference once the disclosure is on", () => {
    expect(effectiveProductMode("visual", true)).toBe("visual");
    expect(effectiveProductMode("ai", true)).toBe("ai");
  });

  it("remembers rather than resets across a hide/show cycle", () => {
    const stored = "visual" as const;
    expect(effectiveProductMode(stored, true)).toBe("visual");
    expect(effectiveProductMode(stored, false)).toBe("ai"); // hidden
    expect(effectiveProductMode(stored, true)).toBe("visual"); // shown again
  });
});

describe("disclosure storage keys", () => {
  it("keeps the legacy key for `advanced` so existing opt-ins survive", () => {
    // The context replaced AdvancedAutomationContext. Changing this key would
    // silently hide the advanced surfaces from every user who had turned them
    // on — a migration-free refactor is only migration-free if the key matches.
    expect(DISCLOSURE_STORAGE_KEYS.advanced).toBe("showAdvancedAutomation");
    expect(DISCLOSURE_STORAGE_KEYS.visual).toBe("showVisualAutomation");
    expect(DISCLOSURE_STORAGE_KEYS.visual).not.toBe(DISCLOSURE_STORAGE_KEYS.advanced);
  });
});
