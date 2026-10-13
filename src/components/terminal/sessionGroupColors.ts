/**
 * Group color ROLE → the CSS custom property it renders with (D4 of plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`).
 *
 * The store records a role, never a color value; each role is named for the
 * `@qontinui/design-tokens` token it resolves from, and these are that
 * token's `--color-*` aliases in `src/index.css`. None is an attention hue.
 */

import type { SessionGroupColorRole } from "./sessionOrgTypes";

export const SESSION_GROUP_COLOR_VAR: Readonly<Record<SessionGroupColorRole, string>> = {
  "brand-primary": "var(--color-brand-primary)",
  "brand-secondary": "var(--color-brand-secondary)",
  "brand-success": "var(--color-brand-success)",
  info: "var(--color-info)",
  "border-strong": "var(--color-border-ds-strong)",
  "text-muted": "var(--color-text-muted)",
};
