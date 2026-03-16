import { createContext, useContext, useState, useCallback } from "react";

export type ProductMode = "ai" | "visual";

const STORAGE_KEY = "qontinui-product-mode";

interface ProductModeContextValue {
  mode: ProductMode;
  setMode: (mode: ProductMode) => void;
}

const ProductModeContext = createContext<ProductModeContextValue>({
  mode: "ai",
  setMode: () => {},
});

export function ProductModeProvider({ children }: { children: React.ReactNode }) {
  const [mode, setModeState] = useState<ProductMode>(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY);
      if (stored === "ai" || stored === "visual") return stored;
    } catch {
      // Ignore
    }
    return "ai";
  });

  const setMode = useCallback((newMode: ProductMode) => {
    setModeState(newMode);
    localStorage.setItem(STORAGE_KEY, newMode);
  }, []);

  return (
    <ProductModeContext.Provider value={{ mode, setMode }}>{children}</ProductModeContext.Provider>
  );
}

export function useProductMode() {
  return useContext(ProductModeContext);
}
