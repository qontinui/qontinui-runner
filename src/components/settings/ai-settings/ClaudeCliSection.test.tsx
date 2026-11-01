/**
 * The Claude CLI section under a fleet account-selection policy (plan
 * 2026-10-01-fleet-account-selection-effective-mode-visibility-and-pin-safe-saves,
 * Phase 2).
 *
 * The DESCRIPTIVE branches (the least-usage explainer, the manual radio list)
 * must follow the EFFECTIVE mode, while the selector buttons and the manual
 * config-dir chooser keep editing the LOCAL mode — and both say the fleet is
 * in charge when it is.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the
 * component is rendered with `renderToStaticMarkup` (effects do not run).
 */

import { describe, it, expect, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

import { ClaudeCliSection } from "./ClaudeCliSection";
import { DEFAULT_AI_SETTINGS } from "./types";
import type { AccountSelectionMode, AiSettings, ClaudeCliSettings } from "../types";

const DIRS = ["/home/u/.claude-work", "/home/u/.claude-personal"];

function render(opts: {
  local: AccountSelectionMode;
  pinned: boolean;
  fleet: AccountSelectionMode | null;
  cli?: Partial<ClaudeCliSettings>;
}) {
  const settings: AiSettings = {
    ...DEFAULT_AI_SETTINGS,
    claude_cli: {
      ...DEFAULT_AI_SETTINGS.claude_cli,
      account_selection_mode: opts.local,
      account_selection_pinned: opts.pinned,
      config_dir: DIRS[0],
      ...opts.cli,
    },
    fleet_account_selection_mode: opts.fleet,
    effective_account_selection_mode: opts.pinned ? opts.local : (opts.fleet ?? opts.local),
    account_selection_source: opts.pinned ? "pinned" : opts.fleet ? "fleet" : "local",
  };
  return renderToStaticMarkup(
    <ClaudeCliSection
      settings={settings}
      setSettings={() => {}}
      onLog={() => {}}
      cliAuth={null}
      refreshingAuth={false}
      onRefreshAuth={() => {}}
      onCheckAuth={() => {}}
      claudeConfigDirs={DIRS}
      setClaudeConfigDirs={() => {}}
      accountUsages={[]}
      setAccountUsages={() => {}}
    />,
  );
}

describe("ClaudeCliSection under a fleet account-selection policy", () => {
  it("a fleet-managed machine describes the fleet's mode, not its local one", () => {
    const html = render({ local: "manual", pinned: false, fleet: "least_usage" });

    // The badge beside the selector names the EFFECTIVE mode.
    expect(html).toContain('data-testid="account-selection-fleet-managed"');
    expect(html).toMatch(
      /data-testid="account-selection-fleet-managed"[^>]*>Managed by fleet policy: (<!-- -->)?Least Usage</,
    );
    // Descriptive branches follow the effective mode: the least-usage explainer
    // shows, and the manual radio list (a manual-mode-only control) does not.
    expect(html).toContain('data-testid="auto-select-rule-caption"');
    expect(html).not.toContain('name="manual_account"');
    // The chooser still edits the LOCAL (manual) mode, carrying the note.
    expect(html).toContain('id="claude-cli-custom-config-dir"');
    expect(html).toContain('data-testid="manual-chooser-fleet-managed"');
  });

  it("a pinned machine describes its local mode and shows no fleet notes", () => {
    const html = render({ local: "manual", pinned: true, fleet: "least_usage" });

    expect(html).not.toContain("account-selection-fleet-managed");
    expect(html).not.toContain("manual-chooser-fleet-managed");
    expect(html).not.toContain('data-testid="auto-select-rule-caption"');
    expect(html).toContain('name="manual_account"');
    expect(html).toContain('id="claude-cli-custom-config-dir"');
  });

  it("with no fleet opinion the local mode governs and no fleet notes show", () => {
    const html = render({ local: "least_usage", pinned: false, fleet: null });

    expect(html).not.toContain("account-selection-fleet-managed");
    expect(html).toContain('data-testid="auto-select-rule-caption"');
    expect(html).not.toContain('name="manual_account"');
    expect(html).not.toContain('id="claude-cli-custom-config-dir"');
  });
});

describe("ClaudeCliSection selection thresholds", () => {
  it("shows a stored override in percentage points and the defaults as placeholders", () => {
    const html = render({
      local: "alternating_handoff",
      pinned: true,
      fleet: null,
      cli: { selection_min_shortfall: 0.035, handoff_load_estimate: 0 },
    });
    expect(html).toContain('data-testid="selection-thresholds"');
    // 0.035 is shown as 3.5 points; an explicit 0 charge stays visible as 0.
    expect(html).toMatch(/aria-label="Minimum shortfall"[^>]*value="3.5"/);
    expect(html).toMatch(/aria-label="Handoff load charge"[^>]*value="0"/);
    // No headroom override: empty, with the built-in 10 points as placeholder.
    expect(html).toMatch(/aria-label="Minimum headroom"[^>]*placeholder="10"[^>]*value=""/);
  });
});
