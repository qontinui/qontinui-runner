/**
 * Geometry Types
 *
 * Re-exports from @qontinui/shared-types for monitor and coordinate handling.
 * Single source of truth: qontinui-schemas/rust/src/geometry.rs.
 *
 * @module geometry
 */

export type {
  Coordinates,
  Region,
  Monitor,
  VirtualDesktop,
  MonitorPosition,
  CoordinateSystem,
} from "@qontinui/shared-types/geometry";

// NOTE: `CoordinateSystem` is now a string-literal union type (not a runtime
// enum) because the canonical Rust type produces that shape via schemars.
// Callers that previously wrote `CoordinateSystem.SCREEN` must switch to the
// literal `"screen"` — grep verified zero such call sites today.

/**
 * @deprecated Use `Monitor` directly. Alias kept for the migration window.
 */
export type { Monitor as MonitorInfo } from "@qontinui/shared-types/geometry";
