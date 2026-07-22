/**
 * Addressability contract for the Session Manager sidebar toggle.
 *
 * `environment: "node"` (no jsdom, no `@testing-library/react`) — so, following
 * `UnzonedChip.test.ts` / `StewardControl.test.tsx`, the exported constant is
 * asserted directly and the JSX glue is asserted by reading the source. The
 * real click path is verified on-page through
 * `POST /ui-bridge/control/element/terminal.session-manager-toggle/action`.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

import { SESSION_MANAGER_TOGGLE_ID } from "./SessionManagerToggle";

const read = (file: string) => readFileSync(fileURLToPath(new URL(file, import.meta.url)), "utf8");

const TOGGLE_SOURCE = read("./SessionManagerToggle.tsx");
const COMMAND_BAR_SOURCE = read("./CommandBar.tsx");

describe("SessionManagerToggle", () => {
  it("uses the `terminal.` namespaced control id the plan pins", () => {
    expect(SESSION_MANAGER_TOGGLE_ID).toBe("terminal.session-manager-toggle");
  });

  it("stamps that id on the button rather than relying on auto-derivation", () => {
    expect(TOGGLE_SOURCE).toContain("data-ui-bridge-id={SESSION_MANAGER_TOGGLE_ID}");
  });

  it('titles the button exactly "Session Manager" so ai/find resolves it', () => {
    expect(TOGGLE_SOURCE).toContain('title="Session Manager"');
    expect(TOGGLE_SOURCE).toContain('aria-label="Session Manager"');
  });

  it("toggles the sidebar state rather than only opening it", () => {
    expect(TOGGLE_SOURCE).toContain("workflowGen.setShowSidebar((v) => !v)");
  });

  it("is mounted in CommandBar — the one always-visible strip of page chrome", () => {
    // StatusStrip, ConductorStatusStrip and UnzonedChip all self-hide, so
    // mounting the toggle in any of them would make it conditionally
    // unreachable. CommandBar's docked footer always renders.
    expect(COMMAND_BAR_SOURCE).toContain("<SessionManagerToggle />");
    expect(COMMAND_BAR_SOURCE).toContain(
      'import { SessionManagerToggle } from "./SessionManagerToggle"',
    );
  });
});
