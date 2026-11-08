// @vitest-environment jsdom
/**
 * The REAL `TenantBadge`, rendered for two zones (plan
 * `2026-11-08-tenant-badge-parity-and-oracle-fixtures` D1). The runner's DOM
 * scanner registers one element per `data-ui-bridge-id`, so the badge must
 * carry a per-zone id: one shared id made every zone's badge a single
 * ambiguous control on a multi-zone page.
 *
 * Mocks copied from `zone-grid/zoneLabelRow.test.tsx`: a multi-tenant device
 * (`showSwitcher: true`) so the badge renders, and a session-info read that
 * resolves no stamp and the acting tenant through the credential, so the badge
 * renders from the tenancy block as well as from a spawn stamp.
 */

import { act, createElement, Fragment } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ACTING = "c231d9da-0ca8-4fe4-bd81-0e3d6c20339a";
// Per-test switch for the device's multi-tenant flag (`useTenant().showSwitcher`).
const tenantFlag = vi.hoisted(() => ({ showSwitcher: true }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => new Promise(() => {})) }));
vi.mock("@/contexts/TenantContext", () => ({
  useTenant: () => ({ showSwitcher: tenantFlag.showSwitcher }),
}));
vi.mock("./useSessionInfo", async (orig) => ({
  ...(await orig<typeof import("./useSessionInfo")>()),
  // The real hook reads nothing for an undefined session id (the badge passes
  // `undefined` when there is no Claude session, or when the switcher is hidden).
  useSessionInfo: (claudeSessionId?: string) => ({
    body:
      claudeSessionId === undefined
        ? null
        : {
            tenancy: {
              row: { tenantId: null },
              diverged: false,
              divergence: "agree",
              credential: { status: "resolved", tenantId: ACTING },
              dataPlane: { status: "resolved", tenantId: ACTING },
              display: { status: "resolved", tenantId: ACTING, source: "credential" },
            },
          },
  }),
}));

import { TenantBadge } from "./TenantBadge";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  tenantFlag.showSwitcher = true;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function renderZones(): void {
  act(() => {
    root.render(
      createElement(
        Fragment,
        null,
        // Zone 0: no spawn stamp — renders from the resolved acting tenant.
        createElement(TenantBadge, { claudeSessionId: "sess-0", zoneIndex: 0 }),
        // Zone 1: a stamped tab.
        createElement(TenantBadge, { tenantId: ACTING, claudeSessionId: "sess-1", zoneIndex: 1 }),
      ),
    );
  });
}

describe("TenantBadge per-zone identity", () => {
  it("registers exactly one id per zone", () => {
    renderZones();
    const ids = [
      ...container.querySelectorAll('[data-ui-bridge-id^="terminal-tenant-badge-"]'),
    ].map((el) => el.getAttribute("data-ui-bridge-id"));
    expect(ids).toEqual(["terminal-tenant-badge-0", "terminal-tenant-badge-1"]);
  });

  it("sets data-testid to the same per-zone id", () => {
    renderZones();
    for (const el of container.querySelectorAll("[data-ui-bridge-id]")) {
      expect(el.getAttribute("data-testid")).toBe(el.getAttribute("data-ui-bridge-id"));
    }
    expect(container.querySelectorAll("[data-testid]")).toHaveLength(2);
  });

  it("no element carries the old shared id", () => {
    renderZones();
    expect(container.querySelector('[data-ui-bridge-id="terminal.tenant-badge"]')).toBeNull();
    expect(container.querySelector('[data-testid="tenant-badge"]')).toBeNull();
    expect(container.innerHTML).not.toContain("terminal.tenant-badge");
  });
});

/**
 * The no-session chain the single-zone strip relies on (plan D2): a stamped
 * plain shell has no Claude session, so the badge has no tenancy to read and
 * must render from its spawn stamp alone — and only on a multi-tenant device.
 * `ZoneCell` opens the strip for exactly this case, so a badge that rendered
 * nothing here would leave an empty strip.
 */
describe("TenantBadge with no Claude session (stamped plain shell)", () => {
  function renderShell(): void {
    act(() => {
      root.render(createElement(TenantBadge, { tenantId: ACTING, zoneIndex: 0 }));
    });
  }

  it("renders from the spawn stamp on a multi-tenant device", () => {
    renderShell();
    const badge = container.querySelector('[data-ui-bridge-id="terminal-tenant-badge-0"]');
    expect(badge).not.toBeNull();
    expect(badge?.getAttribute("data-tenant-id")).toBe(ACTING);
  });

  it("renders nothing on a single-binding device", () => {
    tenantFlag.showSwitcher = false;
    renderShell();
    expect(container.innerHTML).toBe("");
  });

  it("renders nothing with neither a stamp nor a session", () => {
    act(() => {
      root.render(createElement(TenantBadge, { zoneIndex: 0 }));
    });
    expect(container.innerHTML).toBe("");
  });
});
