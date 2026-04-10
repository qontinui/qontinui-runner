import { useContext } from "react";
import { TerminalCoreContext, type TerminalCoreContextValue } from "./TerminalCoreContext";

export function useTerminalCore(): TerminalCoreContextValue {
  const ctx = useContext(TerminalCoreContext);
  if (!ctx) {
    throw new Error("useTerminalCore must be used within a TerminalCoreProvider");
  }
  return ctx;
}
