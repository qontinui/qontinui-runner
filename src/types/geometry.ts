/**
 * Geometry Types
 *
 * Re-exports types from qontinui-schemas for monitor and coordinate handling.
 * This provides a single import point for all geometry types in the runner.
 *
 * @module geometry
 */

// Re-export all types from the shared schema
export type { Coordinates, Region, Monitor, VirtualDesktop } from "@qontinui/schemas/geometry";

// Re-export enums (values, not just types)
export { CoordinateSystem } from "@qontinui/schemas/geometry";

/**
 * @deprecated Use Monitor from @qontinui/schemas instead.
 * This alias is provided for backward compatibility during migration.
 */
export type { Monitor as MonitorInfo } from "@qontinui/schemas/geometry";
