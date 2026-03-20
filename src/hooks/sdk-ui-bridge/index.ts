/**
 * SDK UI Bridge sub-hooks — barrel export.
 */

export { useConnection } from "./useConnection";
export { useElements } from "./useElements";
export { useCommands } from "./useCommands";
export { useSnapshotComponents } from "./useSnapshotComponents";
export { useScreenshot } from "./useScreenshot";
export { useAIOperations } from "./useAIOperations";
export { useCaptureSession, consumePendingCaptureScreenshots } from "./useCaptureSession";
export { useStateExploration } from "./useStateExploration";

// Re-export types
export type {
  SdkAppInfo,
  SdkConnectionInfo,
  CommandHistoryEntry,
  ExplorationProgress,
  UseSdkUIBridgeReturn,
  CaptureSessionRef,
} from "./types";
