/**
 * Layout constants for qontinui-runner
 *
 * Centralized layout values for consistent spacing,
 * dimensions, z-index, and elevation throughout the app.
 */

// ============================================
// Sidebar Dimensions
// ============================================

export const SIDEBAR = {
  WIDTH_EXPANDED: 220,
  WIDTH_COLLAPSED: 56,
  TRANSITION_DURATION: 200, // ms
} as const;

// ============================================
// Content Layout
// ============================================

export const CONTENT = {
  /** Default padding for main content areas */
  PADDING: 16,
  /** Maximum width for readable content */
  MAX_WIDTH_PROSE: 720,
  /** Maximum width for wide content */
  MAX_WIDTH_WIDE: 1200,
  /** Gap between major sections */
  SECTION_GAP: 24,
  /** Gap between cards in a grid */
  CARD_GAP: 16,
} as const;

// ============================================
// Z-Index Scale
// Use these instead of arbitrary z-index values
// ============================================

export const Z_INDEX = {
  /** Base level content */
  BASE: 0,
  /** Raised cards, dropdowns that need to appear above siblings */
  DROPDOWN: 10,
  /** Sticky headers, floating buttons */
  STICKY: 20,
  /** Sidebars, navigation drawers */
  SIDEBAR: 30,
  /** Popovers, tooltips */
  POPOVER: 40,
  /** Modal overlays (backdrop) */
  MODAL_BACKDROP: 50,
  /** Modal content */
  MODAL: 60,
  /** Toast notifications */
  TOAST: 70,
  /** Tooltips that need to appear above everything */
  TOOLTIP: 80,
  /** Maximum - for debug overlays, loading screens */
  MAX: 100,
} as const;

// ============================================
// Elevation (Shadow) Scale
// Maps to Tailwind shadow classes
// ============================================

export const ELEVATION = {
  /** Flat - no shadow */
  FLAT: "",
  /** Subtle lift - cards, panels */
  RAISED: "shadow-sm",
  /** Medium elevation - dropdowns, popovers */
  FLOATING: "shadow-md",
  /** High elevation - modals, dialogs */
  OVERLAY: "shadow-lg",
  /** Maximum elevation - important overlays */
  PROMINENT: "shadow-xl",
} as const;

// ============================================
// Animation Durations
// ============================================

export const DURATION = {
  /** Instant - no perceptible delay */
  INSTANT: 0,
  /** Fast - micro-interactions */
  FAST: 100,
  /** Normal - most transitions */
  NORMAL: 200,
  /** Slow - complex animations */
  SLOW: 300,
  /** Very slow - page transitions */
  SLOWER: 500,
} as const;

// ============================================
// Border Radius
// Maps to Tailwind rounded classes
// ============================================

export const RADIUS = {
  /** No rounding */
  NONE: "rounded-none",
  /** Subtle rounding */
  SM: "rounded-sm",
  /** Default rounding */
  DEFAULT: "rounded",
  /** Medium rounding */
  MD: "rounded-md",
  /** Large rounding - cards, panels */
  LG: "rounded-lg",
  /** Extra large rounding */
  XL: "rounded-xl",
  /** Full rounding - pills, avatars */
  FULL: "rounded-full",
} as const;

// ============================================
// Spacing Scale
// Common spacing values in pixels
// ============================================

export const SPACING = {
  /** 4px */
  XS: 4,
  /** 8px */
  SM: 8,
  /** 12px */
  MD: 12,
  /** 16px */
  LG: 16,
  /** 24px */
  XL: 24,
  /** 32px */
  XXL: 32,
  /** 48px */
  XXXL: 48,
} as const;

// ============================================
// Component-Specific Constants
// ============================================

export const COMPONENTS = {
  /** Log viewer line height */
  LOG_LINE_HEIGHT: 18,
  /** Table header height */
  TABLE_HEADER_HEIGHT: 45,
  /** Default input height */
  INPUT_HEIGHT: 36,
  /** Small input height */
  INPUT_HEIGHT_SM: 28,
  /** Large input height */
  INPUT_HEIGHT_LG: 44,
  /** Button height */
  BUTTON_HEIGHT: 36,
  /** Small button height */
  BUTTON_HEIGHT_SM: 28,
  /** Large button height */
  BUTTON_HEIGHT_LG: 44,
  /** Icon size - small */
  ICON_SM: 16,
  /** Icon size - default */
  ICON_DEFAULT: 20,
  /** Icon size - large */
  ICON_LG: 24,
} as const;

// ============================================
// Breakpoints (for reference)
// These match Tailwind's default breakpoints
// ============================================

export const BREAKPOINTS = {
  SM: 640,
  MD: 768,
  LG: 1024,
  XL: 1280,
  "2XL": 1536,
} as const;
