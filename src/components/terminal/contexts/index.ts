// Phase 4 — plan 2026-05-22-coord-native-session-coordination.
//
// The previous 4-way split (TerminalCore + SessionState + ShellInfra +
// AiFeatures) collapses into a single TerminalSessionContext. The other
// three orthogonal UI contexts (TransitionEffects, UIState, ZoneMetadata)
// stay separate.

export { TerminalSessionProvider, useTerminalSession } from "./TerminalSessionContext";
export type { TerminalSessionContextValue } from "./TerminalSessionContext";

export { ZoneMetadataProvider } from "./ZoneMetadataContext";
export type { ZoneMetadataContextValue } from "./ZoneMetadataContext";
export { useZoneMetadata } from "./useZoneMetadata";

export { TransitionEffectsProvider } from "./TransitionEffectsContext";
export type { TransitionEffectsContextValue } from "./TransitionEffectsContext";
export { useTransitionEffects } from "./useTransitionEffects";

export { UIStateProvider } from "./UIStateContext";
export type { UIStateContextValue } from "./UIStateContext";
export { useUIStateCx } from "./useUIStateCx";
