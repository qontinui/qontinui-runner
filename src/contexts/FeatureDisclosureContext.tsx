import { createContext, useCallback, useContext, useMemo, useState } from "react";

/**
 * Optional feature sets the runner keeps out of the default UI.
 *
 * The runner's default surface is Terminal-first: a session-driven user gets
 * the Terminal, the Productivity board, their run/session history and the
 * memory/knowledge surfaces — and nothing else. Two older, larger paradigms
 * still ship in the app and are revealed one toggle at a time from
 * Settings → General:
 *
 *   - "advanced" — the verification-agentic LOOP WORKFLOW paradigm: the
 *     natural-language Home prompt that composes a
 *     setup→verification→agentic→completion workflow, the Active dashboard
 *     that drives it, the workflow/DAG/step builders, the specs + regression
 *     surfaces, and the loop-only run detail tabs. Wired into
 *     `@qontinui/navigation` via `setShowHiddenItems`, which reveals every
 *     item flagged `hidden` for the "runner" platform.
 *
 *   - "visual" — VISUAL GUI AUTOMATION: image matching, screen capture, state
 *     machines, device (ADB) control. This one is not a per-item flag but a
 *     whole product mode — enabling it reveals the AI Dev / Visual switcher in
 *     the sidebar, which in turn swaps the entire nav to the `productMode:
 *     "visual"` item set. With the disclosure off the switcher is not
 *     rendered AND the product mode is pinned to "ai", so no visual-mode item
 *     can leak into the sidebar through a stale persisted mode.
 *
 * Neither disclosure changes what is REGISTERED. Every route, tab id and
 * settings sub-tab stays reachable by deep-link, UI Bridge `activate-tab` and
 * programmatic navigation regardless of these flags — they gate presentation
 * only. That is what lets the Terminal's "save as workflow" disclosure open
 * the builder without the user first flipping a setting.
 *
 * Persisted per-disclosure in localStorage. Both default OFF.
 */
export type FeatureDisclosure = "advanced" | "visual";

/**
 * localStorage key per disclosure. `advanced` keeps its original
 * `showAdvancedAutomation` key so an existing opt-in survives this refactor —
 * a user who had the advanced surfaces revealed does not silently lose them.
 */
export const DISCLOSURE_STORAGE_KEYS: Record<FeatureDisclosure, string> = {
  advanced: "showAdvancedAutomation",
  visual: "showVisualAutomation",
};

/** Human-readable labels, used by the Settings toggles and log lines. */
export const DISCLOSURE_LABELS: Record<FeatureDisclosure, string> = {
  advanced: "Advanced automation features",
  visual: "Visual GUI automation",
};

interface FeatureDisclosureContextValue {
  /** Is this optional feature set revealed? */
  isEnabled: (disclosure: FeatureDisclosure) => boolean;
  /** Reveal or hide an optional feature set (persisted). */
  setEnabled: (disclosure: FeatureDisclosure, enabled: boolean) => void;
}

function readStored(disclosure: FeatureDisclosure): boolean {
  try {
    return localStorage.getItem(DISCLOSURE_STORAGE_KEYS[disclosure]) === "true";
  } catch {
    // Storage unavailable (private mode, non-DOM test env) — default OFF.
    return false;
  }
}

const FeatureDisclosureContext = createContext<FeatureDisclosureContextValue>({
  isEnabled: () => false,
  setEnabled: () => {},
});

export function FeatureDisclosureProvider({ children }: { children: React.ReactNode }) {
  const [enabled, setEnabledState] = useState<Record<FeatureDisclosure, boolean>>(() => ({
    advanced: readStored("advanced"),
    visual: readStored("visual"),
  }));

  const setEnabled = useCallback((disclosure: FeatureDisclosure, next: boolean) => {
    setEnabledState((prev) => ({ ...prev, [disclosure]: next }));
    try {
      localStorage.setItem(DISCLOSURE_STORAGE_KEYS[disclosure], next ? "true" : "false");
    } catch {
      // Non-persistent is still correct for this session.
    }
  }, []);

  const value = useMemo<FeatureDisclosureContextValue>(
    () => ({
      isEnabled: (disclosure) => enabled[disclosure],
      setEnabled,
    }),
    [enabled, setEnabled],
  );

  return (
    <FeatureDisclosureContext.Provider value={value}>{children}</FeatureDisclosureContext.Provider>
  );
}

export function useFeatureDisclosure() {
  return useContext(FeatureDisclosureContext);
}
