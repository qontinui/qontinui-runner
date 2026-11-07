/**
 * Addressability and placement contract for the terminal work-mode toggle
 * (plan `2026-10-11-runner-terminal-tmux-work-mode`, Phase 4).
 *
 * `environment: "node"` with no DOM renderer, so — following
 * `SessionManagerToggle.test.ts` — the exported constant is asserted directly
 * and the JSX glue by reading the source. The rule the click applies is
 * covered behaviourally by `terminalWorkModeControl.test.ts`.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { TERMINAL_WORK_MODE_TOGGLE_ID } from "./TerminalWorkModeToggle";

const read = (file: string) => readFileSync(fileURLToPath(new URL(file, import.meta.url)), "utf8");

const TOGGLE_SOURCE = read("./TerminalWorkModeToggle.tsx");
const COMMAND_BAR_SOURCE = read("./CommandBar.tsx");

describe("TerminalWorkModeToggle", () => {
  it("uses a `terminal.` namespaced control id", () => {
    expect(TERMINAL_WORK_MODE_TOGGLE_ID).toBe("terminal.work-mode-toggle");
  });

  it("stamps that id on the button and exposes the raw mode as readable state", () => {
    expect(TOGGLE_SOURCE).toContain("data-ui-bridge-id={TERMINAL_WORK_MODE_TOGGLE_ID}");
    expect(TOGGLE_SOURCE).toContain("data-terminal-work-mode={mode}");
  });

  it("shows the current mode by name", () => {
    expect(TOGGLE_SOURCE).toContain("<span>{workModeLabel(mode)}</span>");
  });

  it("toggles through the shared rule, which opens the session manager on entry", () => {
    expect(TOGGLE_SOURCE).toContain("applyWorkModeRequest(");
    expect(TOGGLE_SOURCE).toContain('"toggle",');
    expect(TOGGLE_SOURCE).toContain("setShowSidebar: workflowGen.setShowSidebar");
  });

  it("takes its colours from design-system tokens, not literals", () => {
    expect(TOGGLE_SOURCE).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(TOGGLE_SOURCE).toContain("text-brand-primary");
    expect(TOGGLE_SOURCE).toContain("text-text-muted");
  });

  it("sits immediately after SessionManagerToggle in the CommandBar footer", () => {
    expect(COMMAND_BAR_SOURCE).toContain(
      'import { TerminalWorkModeToggle } from "./TerminalWorkModeToggle"',
    );
    const managerAt = COMMAND_BAR_SOURCE.indexOf("<SessionManagerToggle />");
    const modeAt = COMMAND_BAR_SOURCE.indexOf("<TerminalWorkModeToggle />");
    const pickerAt = COMMAND_BAR_SOURCE.indexOf("<SpawnTenantPicker");
    expect(managerAt).toBeGreaterThan(-1);
    expect(modeAt).toBeGreaterThan(managerAt);
    // Nothing else is mounted between the two controls.
    expect(pickerAt).toBeGreaterThan(modeAt);
    const between = COMMAND_BAR_SOURCE.slice(managerAt + "<SessionManagerToggle />".length, modeAt);
    expect(between).not.toMatch(/<[A-Za-z]/);
  });
});
