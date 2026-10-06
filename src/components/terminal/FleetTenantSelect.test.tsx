/**
 * The Fleet view's tenant selector (plan
 * `2026-09-29-fleet-view-reads-one-unchosen-tenant-so-a-multi-bound-device-sees-a-fraction-of-its-fleet`,
 * Phase 3).
 *
 * Two properties: at N=1 bound tenants nothing new renders, and a bound tenant
 * whose credential cannot act is labelled — but still offered, so the 401 its
 * read gets is shown rather than hidden.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the
 * component is rendered with `renderToStaticMarkup`.
 */

import { describe, it, expect, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { TenantCandidateCredential } from "@/contexts/TenantContext";
import {
  FLEET_PICKER_TENANT_SELECT_ID,
  FLEET_TENANT_ALL_VALUE,
  FleetTenantSelect,
  defaultFleetTenantChoice,
  fleetTenantCredentialNote,
  fleetTenantOptions,
  fleetWalkTenants,
  isAllTenants,
} from "./FleetTenantSelect";

const A = "aaaaaaaa-0000-4000-8000-00000000000a";
const B = "bbbbbbbb-0000-4000-8000-00000000000b";
const C = "cccccccc-0000-4000-8000-00000000000c";

function cred(
  tenant: string,
  can_act: boolean | null,
  slot: string,
  via_default_slot = false,
): TenantCandidateCredential {
  return { tenant, can_act, slot, via_default_slot };
}

function render(showSwitcher: boolean, selected: string | null = null): string {
  return renderToStaticMarkup(
    <FleetTenantSelect
      showSwitcher={showSwitcher}
      candidates={[A, B, C]}
      credentials={[cred(A, true, "usable"), cred(B, false, "present-but-dead")]}
      selected={selected}
      onChange={() => {}}
    />,
  );
}

describe("FleetTenantSelect", () => {
  it("renders nothing on a device with one binding (showSwitcher false)", () => {
    expect(render(false)).toBe("");
  });

  it("renders one option per bound tenant plus all-tenants and the device default", () => {
    const html = render(true);
    expect(html).toContain(`data-ui-bridge-id="${FLEET_PICKER_TENANT_SELECT_ID}"`);
    expect(html).toContain("device default");
    expect(html).toContain(`value="${FLEET_TENANT_ALL_VALUE}"`);
    expect(html).toContain("all tenants (3)");
    expect(html.match(/<option/g)?.length).toBe(5);
    for (const t of [A, B, C]) expect(html).toContain(`value="${t}"`);
  });

  it("labels each option with its short id and its credential state", () => {
    const html = render(true);
    expect(html).toContain(">aaaaaaaa<");
    expect(html).toContain("bbbbbbbb — credential expired");
    // No entry served for C: UNKNOWN, never assumed usable.
    expect(html).toContain("cccccccc — unknown");
  });

  it("keeps an unusable tenant selectable", () => {
    expect(render(true)).not.toContain("disabled");
  });
});

describe("fleetTenantCredentialNote", () => {
  it.each([
    [cred(A, true, "usable"), null],
    // The legacy default slot serves it: works, no re-pair owed.
    [cred(A, true, "absent", true), null],
    [cred(A, false, "absent"), "pair this tenant"],
    [cred(A, false, "present-but-dead"), "credential expired"],
    [cred(A, null, "unreadable"), "unknown"],
    [undefined, "unknown"],
  ])("%j -> %s", (c, want) => {
    expect(fleetTenantCredentialNote(c)).toBe(want);
  });

  it("matches credentials to candidates by tenant, not by position", () => {
    const opts = fleetTenantOptions([A, B], [cred(B, false, "absent"), cred(A, true, "usable")]);
    expect(opts.map((o) => o.note)).toEqual([null, "pair this tenant"]);
  });
});

describe("the merged choice (Phase 4)", () => {
  it("opens on all tenants only when UNPINNED and bound to more than one", () => {
    expect(defaultFleetTenantChoice("unpinned", [A, B])).toBe(FLEET_TENANT_ALL_VALUE);
    // A pin is a choice someone made; it wins.
    expect(defaultFleetTenantChoice("pinned", [A, B])).toBeNull();
    // Unresolvable, an unserved posture, and N<=1 keep Phase 3's default.
    expect(defaultFleetTenantChoice("unresolvable", [A, B])).toBeNull();
    expect(defaultFleetTenantChoice(null, [A, B])).toBeNull();
    expect(defaultFleetTenantChoice("unpinned", [A])).toBeNull();
    expect(defaultFleetTenantChoice("unpinned", [])).toBeNull();
  });

  it("walks one tenant per candidate for all, and exactly the choice otherwise", () => {
    expect(fleetWalkTenants(FLEET_TENANT_ALL_VALUE, [A, B, C])).toEqual([A, B, C]);
    expect(fleetWalkTenants(A, [A, B])).toEqual([A]);
    expect(fleetWalkTenants(null, [A, B])).toEqual([null]);
  });

  it("degrades all-with-no-candidates to the runner's default, never to zero walks", () => {
    expect(fleetWalkTenants(FLEET_TENANT_ALL_VALUE, [])).toEqual([null]);
  });

  it("marks the merged option selected when chosen", () => {
    expect(render(true, FLEET_TENANT_ALL_VALUE)).toMatch(/<option value="\*"[^>]*selected=""/);
    expect(isAllTenants(FLEET_TENANT_ALL_VALUE)).toBe(true);
    expect(isAllTenants(A)).toBe(false);
    expect(isAllTenants(null)).toBe(false);
  });
});
