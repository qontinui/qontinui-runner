import { useContext } from "react";
import { AiFeaturesContext, type AiFeaturesContextValue } from "./AiFeaturesContext";

export function useAiFeatures(): AiFeaturesContextValue {
  const ctx = useContext(AiFeaturesContext);
  if (!ctx) {
    throw new Error("useAiFeatures must be used within an AiFeaturesProvider");
  }
  return ctx;
}
