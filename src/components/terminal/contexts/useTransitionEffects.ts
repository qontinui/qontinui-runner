import { useContext } from "react";
import {
  TransitionEffectsContext,
  type TransitionEffectsContextValue,
} from "./TransitionEffectsContext";

export function useTransitionEffects(): TransitionEffectsContextValue {
  const ctx = useContext(TransitionEffectsContext);
  if (!ctx) {
    throw new Error("useTransitionEffects must be used within a TransitionEffectsProvider");
  }
  return ctx;
}
