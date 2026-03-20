/**
 * Shared types for the SDK UI Bridge sub-hooks.
 */

import type {
  ExternalElement,
  ElementFingerprint,
  ConnectionStatus,
  CommandResult,
  PageScreenshot,
  CaptureRecord,
  CaptureSessionStatus,
  CooccurrenceExport,
} from "../../types/ui-bridge-types";

// Re-export so sub-hooks can import from one place
export type {
  ExternalElement,
  ElementFingerprint,
  ConnectionStatus,
  CommandResult,
  PageScreenshot,
  CaptureRecord,
  CaptureSessionStatus,
  CooccurrenceExport,
};

export interface SdkAppInfo {
  appId: string;
  appName: string;
  appType: string;
  framework?: string;
  version?: string;
  capabilities: string[];
  port: number;
}

/** Information about a single SDK connection */
export interface SdkConnectionInfo {
  url: string;
  app: SdkAppInfo;
  connectedAt: number;
  isActive: boolean;
}

export interface CommandHistoryEntry {
  id: number;
  timestamp: number;
  action: string;
  params?: Record<string, unknown>;
  result: CommandResult;
}

export interface ExplorationProgress {
  current: number;
  total: number;
  currentElement?: string;
}

export interface UseSdkUIBridgeReturn {
  // Connection
  connectionStatus: ConnectionStatus;
  connectedApp: SdkAppInfo | null;
  error: string | null;
  connect: (url: string, appInfo?: Partial<SdkAppInfo>) => Promise<boolean>;
  disconnect: (url?: string) => Promise<void>;

  // Multi-connection
  connections: Map<string, SdkAppInfo>;
  switchConnection: (url: string) => Promise<boolean>;

  // Elements
  elements: ExternalElement[];
  selectedElementId: string | null;
  selectedElement: ExternalElement | null;
  selectElement: (id: string | null) => void;
  refreshElements: () => Promise<void>;
  isLoadingElements: boolean;

  // Actions
  executeAction: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<CommandResult>;
  lastCommandResult: CommandResult | null;
  commandHistory: CommandHistoryEntry[];
  clearCommandHistory: () => void;

  // Snapshot
  snapshot: unknown | null;
  refreshSnapshot: () => Promise<void>;

  // Components
  components: unknown[];
  refreshComponents: () => Promise<void>;

  // Raw API (sendCommand for RawApiPanel compatibility)
  sendCommand: <T = unknown>(
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<CommandResult<T>>;

  // AI
  aiSearch: (query: string) => Promise<unknown>;
  aiExecute: (instruction: string) => Promise<unknown>;

  // Screenshot
  pageScreenshot: PageScreenshot | null;
  isCapturingScreenshot: boolean;
  captureScreenshot: (monitor?: number) => Promise<void>;

  // Highlight
  highlightElement: (elementId: string) => Promise<void>;

  // Fingerprinting & Capture Sessions
  captureSession: CaptureSessionStatus;
  startCaptureSession: () => void;
  stopCaptureSession: () => void;
  cooccurrenceData: CooccurrenceExport | null;
  isLoadingCooccurrence: boolean;
  generateCooccurrenceExport: () => Promise<CooccurrenceExport | null>;

  // State exploration
  exploreStates: () => Promise<CooccurrenceExport | null>;
  isExploring: boolean;
  explorationProgress: ExplorationProgress | null;
  cancelExploration: () => void;
}

/** Internal ref type for capture session data */
export interface CaptureSessionRef {
  sessionId: string;
  startedAt: number;
  captures: CaptureRecord[];
  fingerprintCatalog: Record<string, ElementFingerprint>;
  lastCaptureId: string | null;
  /** Element bounds keyed by fingerprint hash — used for thumbnail cropping */
  elementBoundsMap?: Record<string, { x: number; y: number; width: number; height: number }>;
  /** Cropped element thumbnails keyed by fingerprint hash */
  elementThumbnails?: Record<string, string>;
  /** Full capture screenshots for screenshot state view (deduplicated by fingerprint set) */
  captureScreenshots?: Array<{
    captureIndex: number;
    screenshotBase64: string;
    width: number;
    height: number;
    elementBoundsJson: string;
    fingerprintHashesJson: string;
    capturedAt: string;
  }>;
  /** Previous capture's fingerprint hash set for deduplication */
  prevCaptureHashes?: Set<string>;
}
