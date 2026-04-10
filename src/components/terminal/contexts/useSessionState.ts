import { useContext } from "react";
import { SessionStateContext, type SessionStateContextValue } from "./SessionStateContext";

export function useSessionState(): SessionStateContextValue {
  const ctx = useContext(SessionStateContext);
  if (!ctx) {
    throw new Error("useSessionState must be used within a SessionStateProvider");
  }
  return ctx;
}
