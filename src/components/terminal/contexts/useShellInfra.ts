import { useContext } from "react";
import { ShellInfraContext, type ShellInfraContextValue } from "./ShellInfraContext";

export function useShellInfra(): ShellInfraContextValue {
  const ctx = useContext(ShellInfraContext);
  if (!ctx) {
    throw new Error("useShellInfra must be used within a ShellInfraProvider");
  }
  return ctx;
}
