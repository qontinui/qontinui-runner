/**
 * Design System for qontinui-runner
 *
 * Central export point for all design system utilities.
 * Import from this file for consistency.
 *
 * @example
 * import { getSeverityColors, TYPOGRAPHY, Z_INDEX } from '@/design-system';
 */

// Colors
export {
  getSeverityColors,
  getStatusColors,
  getActionColors,
  getAccentColors,
  type SeverityLevel,
  type SeverityColorClasses,
  type StatusType,
  type StatusColorClasses,
  type ActionType,
  type ActionColorClasses,
  type AccentColor,
  type AccentColorClasses,
} from "./colors";

// Layout
export {
  SIDEBAR,
  CONTENT,
  Z_INDEX,
  ELEVATION,
  DURATION,
  RADIUS,
  SPACING,
  COMPONENTS,
  BREAKPOINTS,
} from "./layout";

// Typography
export {
  TYPOGRAPHY,
  TEXT_COLORS,
  getTypography,
  type TypographyStyle,
  type TypographyVariant,
} from "./typography";
