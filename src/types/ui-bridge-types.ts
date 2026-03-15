/**
 * UI Bridge Types
 *
 * Shared type definitions for UI Bridge elements, fingerprints, and related
 * data structures. These types are used across hooks, components, and lib
 * utilities for element discovery, state machine analysis, and automation.
 *
 * Previously defined in useExternalUIBridge hook; extracted here as standalone
 * types when the Chrome extension was removed.
 */

// =============================================================================
// Element & Fingerprint Types
// =============================================================================

export interface ComputedStyle {
  color?: string;
  backgroundColor?: string;
  fontSize?: string;
  fontWeight?: string;
  fontFamily?: string;
  borderColor?: string;
  borderWidth?: string;
  borderRadius?: string;
  padding?: string;
  margin?: string;
  display?: string;
  position?: string;
  opacity?: string;
  cursor?: string;
  textAlign?: string;
  lineHeight?: string;
  zIndex?: string;
}

export interface RepeatPattern {
  type: string; // 'list' | 'table-row' | 'grid-item' | etc.
  containerTag?: string;
  containerRole?: string;
  index: number;
  count: number;
  itemSelector?: string;
  itemRole?: string;
}

export interface ElementFingerprint {
  // Structural identity (most stable)
  structuralPath: string; // Tag-only path: "nav > ul > li > a"
  positionZone: string; // 'header' | 'footer' | 'sidebar-left' | 'sidebar-right' | 'main' | 'modal' | 'fixed-top' | 'fixed-bottom'
  landmarkContext: string; // Nearest landmark role
  landmarkLabel?: string; // Landmark aria-label if present

  // Semantic identity
  role: string; // ARIA role (explicit or implicit)
  tagName: string;
  accessibleName?: string; // Normalized accessible name

  // Visual identity
  sizeCategory: string; // 'icon' | 'button' | 'small' | 'medium' | 'large' | 'fullwidth' | 'panel'
  relativePosition: {
    top: number; // 0-1 viewport percentage
    left: number;
  };

  // Repeat pattern (for list/grid items)
  isRepeating: boolean;
  repeatPattern?: RepeatPattern;

  // Hash for quick matching
  hash: string;
}

export interface ExternalElement {
  id: string;
  tagName: string;
  type: string;
  bounds: {
    x: number;
    y: number;
    width: number;
    height: number;
    top?: number;
    right?: number;
    bottom?: number;
    left?: number;
  };
  visible: boolean;
  enabled: boolean;
  focused: boolean;
  value?: string;
  checked?: boolean;
  text?: string;
  label?: string;
  parent?: string | null;
  children?: string[];
  actions: string[];
  hasUiId?: boolean;

  // Accessibility object
  accessibility?: {
    role: string;
    accessibleName?: string;
    accessibleDescription?: string;
    ariaLabel?: string;
    ariaLabelledBy?: string;
    ariaDescribedBy?: string;
    ariaExpanded?: boolean;
    ariaSelected?: boolean;
    ariaChecked?: boolean | "mixed";
    ariaHidden?: boolean;
    ariaDisabled?: boolean;
    ariaRequired?: boolean;
    ariaLive?: "off" | "polite" | "assertive";
    tabIndex?: number;
    isInTabOrder?: boolean;
    isKeyboardAccessible?: boolean;
    implicitRole?: string;
    hasExplicitRole?: boolean;
  };

  // Top-level convenience properties (flattened from accessibility)
  role?: string;
  accessibleName?: string;
  is_expanded?: boolean;
  is_pressed?: boolean;
  is_selected?: boolean;
  is_required?: boolean;
  is_readonly?: boolean;
  is_interactive?: boolean;
  ref?: string;
  isCrossOrigin?: boolean;

  // Extraction attributes
  selector?: string;
  xpath?: string;
  classes?: string[];
  href?: string;
  src?: string;
  alt?: string;
  title?: string;
  placeholder?: string;
  name?: string;
  inputType?: string;
  formAction?: string;
  formMethod?: string;
  dataAttributes?: Record<string, string>;
  computedStyle?: ComputedStyle;
  sourceUrl?: string;
  viewportWidth?: number;
  viewportHeight?: number;
  tagIndex?: number;
  depth?: number;
  _discovered?: boolean;

  // Category
  category?: 'interactive' | 'content' | 'media';

  // Media metadata (for media elements)
  mediaMetadata?: {
    mediaType: string;
    src?: string;
    altText?: string;
    isDecorative: boolean;
    naturalWidth?: number;
    naturalHeight?: number;
    renderedWidth: number;
    renderedHeight: number;
    oversizeRatio?: number;
    loadingState: string;
    lazyLoading: boolean;
    format?: string;
    transferSize?: number;
    srcset?: string;
    sizes?: string;
    svgViewBox?: string;
  };

  // Fingerprint for cross-page element matching
  fingerprint?: ElementFingerprint;
}

export interface PageContext {
  url: string;
  title: string;
  elements: ExternalElement[];
  timestamp: number;
}

export type ConnectionStatus = "disconnected" | "connecting" | "connected" | "error";

export interface CommandResult<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
  duration?: number;
}

// =============================================================================
// Screenshot & Capture Types
// =============================================================================

export interface PageScreenshot {
  /** Base64 encoded PNG data (without data URL prefix) */
  data: string;
  /** Timestamp when screenshot was captured */
  capturedAt: number;
  /** Viewport dimensions at capture time */
  viewport: { width: number; height: number };
  /** Scroll info if scroll-based capture was used */
  scrollInfo?: {
    scrolled: boolean;
    newBounds?: {
      x: number;
      y: number;
      width: number;
      height: number;
    } | null;
  } | null;
}

export interface ScrollCaptureOptions {
  elementBounds: {
    x: number;
    y: number;
    width: number;
    height: number;
  };
  restoreScroll?: boolean;
}

export interface FullPageCaptureOptions {
  tileDelay?: number;
  hideFixedElements?: boolean;
}

export interface FullPageScreenshot {
  data: string;
  capturedAt: number;
  totalWidth: number;
  totalHeight: number;
  viewport: { width: number; height: number };
  tilesStitched: number;
  scrollOffset: { x: number; y: number };
}

export interface FullPageTile {
  screenshot: string;
  x: number;
  y: number;
  width: number;
  height: number;
  row: number;
  col: number;
}

export interface FullPageCaptureResult {
  tiles: FullPageTile[];
  totalWidth: number;
  totalHeight: number;
  viewportWidth: number;
  viewportHeight: number;
  capturedAt: number;
  tabId: number;
  url: string;
}

export interface FullPageCaptureProgress {
  currentTile: number;
  totalTiles: number;
  phase: "capturing" | "stitching" | "complete";
}

// =============================================================================
// Capture Session Types (for State Machine Discovery)
// =============================================================================

export interface CaptureRecord {
  captureId: string;
  timestamp: number;
  url: string;
  title: string;
  elementFingerprints: string[];
  elementCount: number;
  triggeredBy?: {
    actionType: string;
    targetFingerprint: string;
    previousCaptureId: string;
  };
}

export interface ActionRecord {
  actionId: string;
  timestamp: number;
  actionType: string;
  targetFingerprint: string;
  beforeCaptureId: string;
  afterCaptureId: string;
  addedFingerprints: string[];
  removedFingerprints: string[];
}

export interface CaptureSessionStatus {
  active: boolean;
  sessionId?: string;
  startedAt?: number;
  captureCount?: number;
  actionCount?: number;
  uniqueFingerprints?: number;
}

export interface CaptureSessionExport {
  sessionId: string;
  startedAt: number;
  endedAt?: number;
  exportedAt?: number;
  captures: CaptureRecord[];
  actions: ActionRecord[];
  fingerprintCatalog: Record<string, ElementFingerprint>;
}

/**
 * Co-occurrence export format optimized for state discovery algorithms.
 */
export interface CooccurrenceExport {
  sessionId: string;
  exportedAt: number;
  allFingerprints: string[];
  fingerprintDetails: Record<string, ElementFingerprint>;
  presenceMatrix: Array<{
    captureId: string;
    captureIndex: number;
    timestamp: number;
    url: string;
    title: string;
    fingerprints: string[];
  }>;
  cooccurrenceCounts: Record<string, Record<string, number>>;
  fingerprintStats: Record<
    string,
    {
      hash: string;
      totalAppearances: number;
      captureIds: string[];
      firstSeen: number;
      lastSeen: number;
    }
  >;
  transitions: Array<{
    actionId: string;
    actionType: string;
    targetFingerprint: string;
    beforeCaptureId: string;
    afterCaptureId: string;
    appearedFingerprints: string[];
    disappearedFingerprints: string[];
    timestamp: number;
  }>;
  stateCandidates: Array<{
    fingerprints: string[];
    cooccurrenceRate: number;
    appearanceCount: number;
  }>;
  /** Optional map of fingerprint hash → base64 PNG thumbnail of the element. */
  elementThumbnails?: Record<string, string>;
}

// =============================================================================
// Browser Tab Types (from useLiveBrowser)
// =============================================================================

export interface BrowserTab {
  id: number;
  url: string;
  title: string;
  active: boolean;
  windowId: number;
  favIconUrl?: string;
}

export interface MobileDevice {
  device_id: string;
  device_type: "emulator" | "physical";
  model?: string;
  state: string;
}

export interface DiscoveredElement {
  id: string;
  tagName: string;
  type: string;
  bounds: {
    x: number;
    y: number;
    width: number;
    height: number;
    top?: number;
    right?: number;
    bottom?: number;
    left?: number;
  };
  visible: boolean;
  enabled: boolean;
  focused: boolean;
  value?: string;
  checked?: boolean;
  text?: string;
  label?: string;
  parent?: string;
  children?: string[];
  actions: string[];
  hasUiId?: boolean;
}

export type TargetType = "browser" | "mobile" | "tauri";

export interface ConnectedTarget {
  type: TargetType;
  id: string | number;
  name: string;
  url?: string;
}
