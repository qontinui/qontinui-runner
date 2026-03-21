import { createContext, useCallback, useContext, useRef } from "react";

interface NavigationContextValue {
  navigate: (page: string) => void;
  registerNavigate: (fn: (page: string) => void) => void;
}

const NavigationContext = createContext<NavigationContextValue | null>(null);

export function NavigationProvider({ children }: { children: React.ReactNode }) {
  const navigateFnRef = useRef<((page: string) => void) | null>(null);

  const navigate = useCallback((page: string) => {
    if (navigateFnRef.current) {
      navigateFnRef.current(page);
    }
  }, []);

  const registerNavigate = useCallback((fn: (page: string) => void) => {
    navigateFnRef.current = fn;
  }, []);

  return (
    <NavigationContext.Provider value={{ navigate, registerNavigate }}>
      {children}
    </NavigationContext.Provider>
  );
}

export function useNavigation() {
  const context = useContext(NavigationContext);
  if (!context) {
    throw new Error("useNavigation must be used within NavigationProvider");
  }
  return context;
}
