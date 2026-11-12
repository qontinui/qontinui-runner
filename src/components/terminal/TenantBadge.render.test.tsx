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
const OTHER = "6b1f4b0e-1111-4000-8000-000000000001";
const ANOTHER = "91ffaa20-2222-4000-8000-000000000002";
// Per-test switch for the device's multi-tenant flag (`useTenant().showSwitcher`).
const tenantFlag = vi.hoisted(() => ({ showSwitcher: true }));
// Per-test switch for whether the runner sent `tenancy.display` (an older
// runner, or a read that has not projected one, sends none).
const displayFlag = vi.hoisted(() => ({ sent: true }));
// Per-test switch for the display's measured membership of the acting tenant
// in the runner's two-tenant bound set (`actingInBoundSet`).
const boundFlag = vi.hoisted(() => ({ actingInBoundSet: true as boolean | null }));

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
              // The full shape `normalizeTenantDisplay` always produces.
              display: displayFlag.sent
                ? {
                    status: "resolved",
                    tenantId: ACTING,
                    tenantSlug: null,
                    source: "credential",
                    reason: null,
                    conflicts: [],
                    conflictDetail: null,
                    // A list consistent with the flag, as the Rust producer
                    // derives it: a tenant measured outside is not listed.
                    bound: {
                      status: "measured",
                      tenantIds:
                        boundFlag.actingInBoundSet === false ? [OTHER, ANOTHER] : [OTHER, ACTING],
                      reason: null,
                    },
                    multiBound: true,
                    actingInBoundSet: boundFlag.actingInBoundSet,
                  }
                : null,
            },
          },
  }),
}));

import { TenantBadge } from "./TenantBadge";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  tenantFlag.showSwitcher = true;
  displayFlag.sent = true;
  boundFlag.actingInBoundSet = true;
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
 * The display attributes (`tenantBadgeDisplayAttrs`) reach the REAL element on
 * the per-zone badge. Their arms are unit-tested in `TenantBadge.test.ts`;
 * this pins the spread itself, which a unit test of the helper cannot see. A
 * UI Bridge read compares these, not the prose `title`.
 */
describe("TenantBadge display attributes on the rendered element", () => {
  it("states the resolved display, and the acting tenant it did not take from the stamp", () => {
    renderZones();
    const badge = container.querySelector('[data-ui-bridge-id="terminal-tenant-badge-0"]');
    expect(badge?.getAttribute("data-tenant-display-status")).toBe("resolved");
    expect(badge?.getAttribute("data-tenant-display-source")).toBe("credential");
    expect(badge?.getAttribute("data-tenant-acting")).toBe(ACTING);
  });

  it("states no display when the runner sent none, rather than inventing one", () => {
    // A stamped session: the badge renders from its stamp, and a tenancy block
    // IS read — it simply carries no display, so no display attribute may appear.
    displayFlag.sent = false;
    act(() => {
      root.render(
        createElement(TenantBadge, { tenantId: ACTING, claudeSessionId: "sess-0", zoneIndex: 0 }),
      );
    });
    const badge = container.querySelector('[data-ui-bridge-id="terminal-tenant-badge-0"]');
    expect(badge?.getAttribute("data-tenant-id")).toBe(ACTING);
    expect(badge?.getAttribute("data-tenant-credential")).toBe(ACTING);
    expect(badge?.hasAttribute("data-tenant-display-status")).toBe(false);
    expect(badge?.hasAttribute("data-tenant-display-source")).toBe(false);
    expect(badge?.hasAttribute("data-tenant-acting")).toBe(false);
    expect(badge?.hasAttribute("data-tenant-bound-membership")).toBe(false);
  });
});

/**
 * The bound-set half of the display reaches the rendered badge, as the
 * session-info row's suffix does (`not among N bound`): an acting tenant
 * MEASURED outside the runner's bound set is never the plain blue label.
 */
describe("TenantBadge bound-set membership on the rendered element", () => {
  const BLUE = "text-[#7aa2f7]";
  const AMBER = "text-[#e0af68]";

  it("an acting tenant measured outside the bound set renders in the attention tone and says so", () => {
    boundFlag.actingInBoundSet = false;
    renderZones();
    const badge = container.querySelector('[data-ui-bridge-id="terminal-tenant-badge-0"]');
    expect(badge?.className).toContain(AMBER);
    expect(badge?.className).not.toContain(BLUE);
    expect(badge?.getAttribute("data-tenant-bound-membership")).toBe("out");
    expect(badge?.getAttribute("title")).toContain(
      "Tenant c231d9da is NOT among the 2 tenants this runner is bound to.",
    );
    expect(badge?.getAttribute("aria-label")).toBe(badge?.getAttribute("title"));
  });

  it("a measured member keeps the plain tone and states its membership", () => {
    renderZones();
    const badge = container.querySelector('[data-ui-bridge-id="terminal-tenant-badge-0"]');
    expect(badge?.className).toContain(BLUE);
    expect(badge?.getAttribute("data-tenant-bound-membership")).toBe("in");
    expect(badge?.getAttribute("title")).toContain(
      "Tenant c231d9da is 1 of the 2 tenants this runner is bound to.",
    );
  });

  it("an unknown membership is said, and is not the attention tone of a measured outside", () => {
    boundFlag.actingInBoundSet = null;
    renderZones();
    const badge = container.querySelector('[data-ui-bridge-id="terminal-tenant-badge-0"]');
    expect(badge?.className).toContain(BLUE);
    expect(badge?.getAttribute("data-tenant-bound-membership")).toBe("unknown");
    expect(badge?.getAttribute("title")).toContain(
      "Whether tenant c231d9da is among the 2 tenants this runner is bound to is unknown.",
    );
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
