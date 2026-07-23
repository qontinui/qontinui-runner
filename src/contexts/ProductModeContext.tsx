import { createContext, useContext, useState, useCallback, useMemo } from "react";
import { useFeatureDisclosure } from "./FeatureDisclosureContext";

export type ProductMode = "ai" | "visual";

const STORAGE_KEY = "qontinui-product-mode";

interface ProductModeContextValue {
  /**
   * The EFFECTIVE mode. Pinned to "ai" whenever the "visual" feature
   * disclosure is off, so a persisted `"visual"` from an earlier session can
   * never resurrect the visual sidebar for a user who has since hidden it.
   */
  mode: ProductMode;
  setMode: (mode: ProductMode) => void;
  /**
   * Whether the AI Dev / Visual switcher should be offered at all. False
   * until the user enables "Show visual GUI automation" in Settings → General
   * — visual automation is an optional paradigm, so its entry point is itself
   * disclosure-gated rather than permanently occupying the sidebar header.
   */
  canSwitchMode: boolean;
}

/**
 * The mode actually in force.
 *
 * Pure and exported so the pin can be tested without mounting a provider: the
 * whole point of deriving rather than storing is that a persisted `"visual"`
 * from an earlier session cannot resurrect the visual sidebar for a user who
 * has since hidden it — and that the preference is REMEMBERED, so re-enabling
 * the disclosure restores their last mode instead of resetting them to AI Dev.
 */
export function effectiveProductMode(stored: ProductMode, canSwitchMode: boolean): ProductMode {
  return canSwitchMode ? stored : "ai";
}

const ProductModeContext = createContext<ProductModeContextValue>({
  mode: "ai",
  setMode: () => {},
  canSwitchMode: false,
});

export function ProductModeProvider({ children }: { children: React.ReactNode }) {
  const { isEnabled } = useFeatureDisclosure();
  const canSwitchMode = isEnabled("visual");

  const [storedMode, setStoredMode] = useState<ProductMode>(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY);
      if (stored === "ai" || stored === "visual") return stored;
    } catch {
      // Ignore
    }
    return "ai";
  });

  const setMode = useCallback((newMode: ProductMode) => {
    setStoredMode(newMode);
    try {
      localStorage.setItem(STORAGE_KEY, newMode);
    } catch {
      // Ignore
    }
  }, []);

  const mode = effectiveProductMode(storedMode, canSwitchMode);

  const value = useMemo(
    () => ({ mode, setMode, canSwitchMode }),
    [mode, setMode, canSwitchMode],
  );

  return <ProductModeContext.Provider value={value}>{children}</ProductModeContext.Provider>;
}

export function useProductMode() {
  return useContext(ProductModeContext);
}
