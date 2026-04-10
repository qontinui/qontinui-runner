import { useContext } from "react";
import { UIStateContext, type UIStateContextValue } from "./UIStateContext";

export function useUIStateCx(): UIStateContextValue {
  const ctx = useContext(UIStateContext);
  if (!ctx) {
    throw new Error("useUIStateCx must be used within a UIStateProvider");
  }
  return ctx;
}
