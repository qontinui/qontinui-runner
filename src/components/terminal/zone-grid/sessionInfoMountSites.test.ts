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
 *   2. single / maximized    `ZoneGrid.tsx`                 → mounts
 *   3. solo session strip    `ZoneGrid.tsx`                 → mounts
 *   4. compact zone card     `zone-grid/CompactZoneCard.tsx`→ mounts (this fix)
 *   5. off-grid parking      `zone-grid/HiddenTerminal.tsx` → deliberately NOT
 *
 * (5) is `<div className="hidden">`: a trigger there would be invisible, not
 * keyboard-reachable, and — having no zone — every hidden tab would register
 * the SAME `terminal-session-info-trigger--1` id. Its reachable home is the
 * zone control panel's Unassigned list, which is separate work.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

import { sessionInfoElementId } from "../useSessionInfo";

const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

const COMPACT = read("./CompactZoneCard.tsx");
const ZONE_LABEL = read("./ZoneLabel.tsx");
const HIDDEN = read("./HiddenTerminal.tsx");
const ZONE_GRID = read("../ZoneGrid.tsx");

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
    expect(ZONE_GRID).toContain(`${MOUNT} zoneIndex={singleViewZone} />`);
    expect(ZONE_GRID).toContain(`${MOUNT} zoneIndex={zoneIdx} />`);
  });

  it("does NOT mount inside the off-grid hidden parking slot", () => {
    // A `display: none` subtree is neither visible nor keyboard-reachable, and
    // an unassigned tab has no zone index to key the element ids off. If this
    // ever changes, the id scheme has to grow a non-zone discriminator first.
    expect(HIDDEN).not.toContain("SessionInfoDropdown");
    expect(HIDDEN).toContain('className="hidden"');
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
    expect(ZONE_GRID).toContain("{showCompactCard && (");
    expect(ZONE_GRID).toContain("{!showCompactCard && showLabels && (");
  });
});
