/**
 * Typography system for qontinui-runner
 *
 * Defines consistent text styles across the application.
 * These map to Tailwind utility classes.
 */

// ============================================
// Typography Scale
// Combines size, weight, and line-height
// ============================================

export interface TypographyStyle {
  /** Tailwind class string */
  className: string;
  /** Font size in pixels (for reference) */
  size: number;
  /** Line height ratio */
  lineHeight: number;
  /** Font weight */
  weight: number;
}

export const TYPOGRAPHY = {
  // Headings
  h1: {
    className: "text-2xl font-bold leading-tight",
    size: 24,
    lineHeight: 1.25,
    weight: 700,
  },
  h2: {
    className: "text-xl font-semibold leading-tight",
    size: 20,
    lineHeight: 1.25,
    weight: 600,
  },
  h3: {
    className: "text-lg font-semibold leading-snug",
    size: 18,
    lineHeight: 1.375,
    weight: 600,
  },
  h4: {
    className: "text-base font-semibold leading-normal",
    size: 16,
    lineHeight: 1.5,
    weight: 600,
  },

  // Body text
  body: {
    className: "text-sm leading-normal",
    size: 14,
    lineHeight: 1.5,
    weight: 400,
  },
  bodyLarge: {
    className: "text-base leading-relaxed",
    size: 16,
    lineHeight: 1.625,
    weight: 400,
  },
  bodySmall: {
    className: "text-xs leading-normal",
    size: 12,
    lineHeight: 1.5,
    weight: 400,
  },

  // Labels and captions
  label: {
    className: "text-sm font-medium leading-none",
    size: 14,
    lineHeight: 1,
    weight: 500,
  },
  labelSmall: {
    className: "text-xs font-medium leading-none",
    size: 12,
    lineHeight: 1,
    weight: 500,
  },
  caption: {
    className: "text-xs text-muted-foreground leading-normal",
    size: 12,
    lineHeight: 1.5,
    weight: 400,
  },

  // Code and monospace
  code: {
    className: "text-sm font-mono leading-normal",
    size: 14,
    lineHeight: 1.5,
    weight: 400,
  },
  codeSmall: {
    className: "text-xs font-mono leading-normal",
    size: 12,
    lineHeight: 1.5,
    weight: 400,
  },

  // Special
  stat: {
    className: "text-3xl font-bold tabular-nums leading-none",
    size: 30,
    lineHeight: 1,
    weight: 700,
  },
  statSmall: {
    className: "text-xl font-semibold tabular-nums leading-none",
    size: 20,
    lineHeight: 1,
    weight: 600,
  },
  badge: {
    className: "text-xs font-medium leading-none",
    size: 12,
    lineHeight: 1,
    weight: 500,
  },
} as const;

// ============================================
// Text Color Utilities
// Semantic text colors
// ============================================

export const TEXT_COLORS = {
  /** Primary text - headings, important content */
  primary: "text-foreground",
  /** Secondary text - body, descriptions */
  secondary: "text-muted-foreground",
  /** Muted text - hints, placeholders */
  muted: "text-muted-foreground/70",
  /** Inverted text - on colored backgrounds */
  inverse: "text-background",
  /** Link text */
  link: "text-primary hover:text-primary/80",
  /** Success text */
  success: "text-green-500",
  /** Warning text */
  warning: "text-yellow-500",
  /** Error text */
  error: "text-red-500",
  /** Info text */
  info: "text-blue-400",
} as const;

// ============================================
// Helper function for typography
// ============================================

export type TypographyVariant = keyof typeof TYPOGRAPHY;

export function getTypography(variant: TypographyVariant): string {
  return TYPOGRAPHY[variant].className;
}
