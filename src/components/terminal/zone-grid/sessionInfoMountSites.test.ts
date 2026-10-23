/**
 * Census of the terminal render paths that can host a session, and which of
 * them mount `SessionInfoDropdown`.
 *
 * The dropdown is the ONLY surface answering "which Claude session is this,
 * and what has it opened / landed?". A render path that shows a session but no
 * trigger can never expose that, no matter how correct the backend read is —
 * so the set of mount sites is a contract, not an implementation detail.
 *
 * `environment: "node"` (no jsdom, no `@testing-library/react`), so — following
 * `SessionInfoDropdown.test.ts` / `utils.test.ts` — the JSX glue is asserted by
 * reading the source and the id contract is asserted through the exported
 * helper.
 *
 * The five paths that render a live session:
 *   1. full zone header      `zone-grid/ZoneLabel.tsx`      → mounts
 *   2. maximized header      `zone-grid/MaximizedHeader.tsx`→ mounts
 *   3. solo session strip    `zone-grid/ZoneCell.tsx`       → mounts
 *   4. compact zone card     `zone-grid/CompactZoneCard.tsx`→ mounts (this fix)
 *   5. parked cell           `zone-grid/ZoneCell.tsx`       → deliberately NOT
 *      (an unzoned grid tab, an off-screen tmux tab, or a zone behind the
 *      maximized one: `isParked`)
 *
 * (5) is a `display: none` cell: a trigger there would be invisible, not
 * keyboard-reachable, and — a parked cell's index being no zone on screen —
 * would register the same `terminal-session-info-trigger-<n>` id as a real
 * one. Its reachable homes are the zone control panel's Unassigned list and
 * the session manager. The maximized zone's cell hides the zone chrome too
 * (`hideZoneChrome`); its trigger is the maximized header's.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

import { sessionInfoElementId } from "../useSessionInfo";

const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

const COMPACT = read("./CompactZoneCard.tsx");
const ZONE_LABEL = read("./ZoneLabel.tsx");
const MAXIMIZED_HEADER = read("./MaximizedHeader.tsx");
const ZONE_CELL = read("./ZoneCell.tsx");

/** The one call shape every mount site uses. */
const MOUNT = "<SessionInfoDropdown claudeSessionId={tab.claudeSessionId}";

describe("SessionInfoDropdown mount sites", () => {
  it("mounts in the compact zone card", () => {
    expect(COMPACT).toContain('import { SessionInfoDropdown } from "../SessionInfoDropdown";');
    expect(COMPACT).toContain(`${MOUNT} zoneIndex={zoneIndex} />`);
  });

  it("still mounts in the full zone header", () => {
    expect(ZONE_LABEL).toContain(`${MOUNT} zoneIndex={zoneIndex} />`);
  });

  it("still mounts in the single/maximized header and the solo strip", () => {
    expect(MAXIMIZED_HEADER).toContain(`${MOUNT} zoneIndex={zoneIdx} />`);
    expect(ZONE_CELL).toContain("{isMaximizedPane && maximizedNav && (");
    expect(ZONE_CELL).toContain(`${MOUNT} zoneIndex={zoneIdx} />`);
  });

  it("does NOT mount inside a parked cell", () => {
    // A `display: none` subtree is neither visible nor keyboard-reachable, and
    // a parked tab has no zone index of its own to key the element ids off. If
    // this ever changes, the id scheme has to grow a non-zone discriminator
    // first. Both mount sites in the cell are gated off for a parked cell.
    expect(ZONE_CELL).toContain('const isParked = presentation === "parked";');
    expect(ZONE_CELL).toContain("const hideZoneChrome = isParked || isMaximizedPane;");
    expect(ZONE_CELL).toContain("const showLabels = showLabelsProp && !hideZoneChrome;");
    expect(ZONE_CELL).toMatch(/const soloSessionInfo =\s*!hideZoneChrome &&/);
    expect(ZONE_CELL).toContain('${isParked ? "hidden" : ""}');
  });
});

describe("SessionInfoDropdown addressability across mount sites", () => {
  it("keeps ONE id scheme — the compact card invents no second one", () => {
    // The compact card delegates ALL addressing to the dropdown by handing it a
    // real zone index: it mints no id of its own and stamps no
    // `data-ui-bridge-id`. So a UI-Bridge client addresses a compact zone's
    // session info with exactly the ids it uses for a full zone.
    expect(COMPACT).not.toContain("sessionInfoElementId");
    expect(COMPACT).not.toContain("data-ui-bridge-id=");
    expect(sessionInfoElementId("trigger", 4)).toBe("terminal-session-info-trigger-4");
    expect(sessionInfoElementId("panel", 4)).toBe("terminal-session-info-panel-4");
    expect(sessionInfoElementId("prs-landed", 4)).toBe("terminal-session-info-prs-landed-4");
    expect(sessionInfoElementId("prs-opened", 4)).toBe("terminal-session-info-prs-opened-4");
  });

  it("cannot double-register a zone: the compact card and ZoneLabel are exclusive", () => {
    // `showCompactCard` gates the two apart in the zone cell, and
    // `showSoloSessionInfo` excludes the compact card outright — so at most one
    // of the three mount sites renders for any given `zoneIdx`.
    expect(ZONE_CELL).toContain("{showCompactCard && (");
    expect(ZONE_CELL).toContain("{!showCompactCard && showLabels && (");
  });
});
