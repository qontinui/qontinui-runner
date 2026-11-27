/**
 * Group color ROLE → the CSS custom property it renders with (D4 of plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`).
 *
 * The store records a role, never a color value; each role is named for the
 * `@qontinui/design-tokens` token it resolves from, and these are that
 * token's `--color-*` aliases in `src/index.css`. None is an attention hue.
 */

import { SESSION_GROUP_COLOR_ROLES, type SessionGroupColorRole } from "./sessionOrgTypes";

export const SESSION_GROUP_COLOR_VAR: Readonly<Record<SessionGroupColorRole, string>> = {
  "brand-primary": "var(--color-brand-primary)",
  "brand-secondary": "var(--color-brand-secondary)",
  "brand-success": "var(--color-brand-success)",
  info: "var(--color-info)",
  "border-strong": "var(--color-border-ds-strong)",
  "text-muted": "var(--color-text-muted)",
};

/**
 * The color for the `index`-th of a set of labels, cycling the six roles in
 * their declared order. Zone tags (`useZoneLabelsAndTags`) color by it, so a
 * zone tag is drawn from the same non-attention palette as a session group
 * and never reads as an error or a warning.
 */
export function groupColorAt(index: number): string {
  const n = SESSION_GROUP_COLOR_ROLES.length;
  return SESSION_GROUP_COLOR_VAR[SESSION_GROUP_COLOR_ROLES[((index % n) + n) % n]];
}

/**
 * A translucent tint of a group color for a background. The colors are CSS
 * custom properties, so a hex alpha suffix (`${color}15`) cannot apply —
 * `color-mix` does.
 */
export function groupColorTint(color: string, percent: number): string {
  return `color-mix(in srgb, ${color} ${percent}%, transparent)`;
}
