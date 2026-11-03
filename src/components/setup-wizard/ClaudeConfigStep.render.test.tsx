// @vitest-environment jsdom
/**
 * The REAL `ClaudeConfigStep`, rendered: finishing the wizard must never call
 * the replace-style `save_claude_config_dirs`, which is how a Linux runner's
 * roster was emptied (coord finding `bc9fdc87`). Only the add-only
 * `add_claude_config_dirs_from_setup` may write, and only with dirs that are
 * not already configured.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const openDialog = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => openDialog(...args),
}));

import { ClaudeConfigStep, type DiscoveredDir } from "./ClaudeConfigStep";

let container: HTMLDivElement;
let root: Root;
let onComplete: ReturnType<typeof vi.fn<() => void>>;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  invoke.mockReset();
  openDialog.mockReset();
  onComplete = vi.fn<() => void>();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

/**
 * Discovery answers `discovered` (or rejects with `discoveryError`); the add
 * resolves, or rejects with `addError`.
 */
function stubBackend(discovered: DiscoveredDir[], discoveryError?: string, addError?: string) {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === "discover_claude_config_dirs") {
      return discoveryError === undefined
        ? Promise.resolve(discovered)
        : Promise.reject(new Error(discoveryError));
    }
    if (cmd === "add_claude_config_dirs_from_setup" && addError !== undefined) {
      return Promise.reject(new Error(addError));
    }
    return Promise.resolve({ added: 0, dirs: [] });
  });
}

async function flush() {
  await act(async () => {
    for (let i = 0; i < 5; i++) await Promise.resolve();
  });
}

async function renderStep() {
  await act(async () => {
    root.render(createElement(ClaudeConfigStep, { onComplete, onBack: () => {} }));
  });
  await flush();
}

function byTestId<T extends HTMLElement>(id: string): T[] {
  return Array.from(container.querySelectorAll<T>(`[data-testid="${id}"]`));
}

async function click(el: HTMLElement) {
  await act(async () => {
    el.click();
  });
  await flush();
}

function commandsCalled(): string[] {
  return invoke.mock.calls.map((c) => c[0] as string);
}

const configuredA: DiscoveredDir = {
  path: "/home/u/.claude-a",
  label: ".claude-a",
  source: "configured",
};
const foundB: DiscoveredDir = {
  path: "/home/u/.claude-b",
  label: ".claude-b",
  source: "auto-detected",
};

describe("ClaudeConfigStep Finish", () => {
  it("with nothing discovered, writes nothing", async () => {
    stubBackend([]);
    await renderStep();

    await click(byTestId("claude-config-finish")[0]);

    expect(commandsCalled()).toEqual(["discover_claude_config_dirs"]);
    expect(onComplete).toHaveBeenCalledTimes(1);
  });

  it("with every new dir unchecked, writes nothing", async () => {
    stubBackend([configuredA, foundB]);
    await renderStep();
    const [aBox, bBox] = byTestId<HTMLInputElement>("claude-config-dir-checkbox");
    expect(aBox.checked && aBox.disabled).toBe(true);
    await click(bBox);
    expect(bBox.checked).toBe(false);

    await click(byTestId("claude-config-finish")[0]);

    expect(commandsCalled()).not.toContain("save_claude_config_dirs");
    expect(commandsCalled()).not.toContain("add_claude_config_dirs_from_setup");
  });

  it("adds only the new checked dirs, through the add-only command", async () => {
    stubBackend([configuredA, foundB]);
    await renderStep();

    await click(byTestId("claude-config-finish")[0]);

    expect(commandsCalled()).not.toContain("save_claude_config_dirs");
    expect(invoke).toHaveBeenCalledWith("add_claude_config_dirs_from_setup", {
      dirs: ["/home/u/.claude-b"],
    });
  });

  it("a configured row cannot be unchecked", async () => {
    stubBackend([configuredA]);
    await renderStep();
    const [aBox] = byTestId<HTMLInputElement>("claude-config-dir-checkbox");

    await click(aBox);

    expect(aBox.checked).toBe(true);
    expect(container.textContent).toContain("Remove it in Settings");
  });

  it("a manual-only selection goes to the add-only command, never the replace-style save", async () => {
    // The backend drops a folder without projects/; the point is that the
    // wizard never hands it to a command that would replace the roster.
    stubBackend([]);
    openDialog.mockResolvedValue("/picked/no-projects");
    await renderStep();

    await click(byTestId("claude-config-add-directory")[0]);
    await click(byTestId("claude-config-finish")[0]);

    expect(commandsCalled()).not.toContain("save_claude_config_dirs");
    expect(invoke).toHaveBeenCalledWith("add_claude_config_dirs_from_setup", {
      dirs: ["/picked/no-projects"],
    });
  });

  it("when discovery cannot read the roster, writes nothing and offers no folder picker", async () => {
    stubBackend([], "claude-accounts.json exists but cannot be read");
    openDialog.mockResolvedValue("/picked/acct");
    await renderStep();
    expect(byTestId("claude-config-discovery-error")).toHaveLength(1);
    const addDirectory = byTestId<HTMLButtonElement>("claude-config-add-directory")[0];
    expect(addDirectory.disabled).toBe(true);

    await click(addDirectory);
    await click(byTestId("claude-config-finish")[0]);

    expect(openDialog).not.toHaveBeenCalled();
    expect(commandsCalled()).toEqual(["discover_claude_config_dirs"]);
    expect(onComplete).toHaveBeenCalledTimes(1);
  });

  it("a refused add is shown inline and the wizard stays on this step", async () => {
    stubBackend(
      [foundB],
      undefined,
      "/cfg/claude-accounts.json exists but cannot be read; refusing to write",
    );
    await renderStep();

    await click(byTestId("claude-config-finish")[0]);

    expect(invoke).toHaveBeenCalledWith("add_claude_config_dirs_from_setup", {
      dirs: ["/home/u/.claude-b"],
    });
    const shown = byTestId("claude-config-add-error");
    expect(shown).toHaveLength(1);
    expect(shown[0].textContent).toContain("refusing to write");
    expect(onComplete).not.toHaveBeenCalled();
    expect(byTestId<HTMLButtonElement>("claude-config-finish")[0].disabled).toBe(false);
  });
});
