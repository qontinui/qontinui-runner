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
  FleetTenantSelect,
  fleetTenantCredentialNote,
  fleetTenantOptions,
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

  it("renders one option per bound tenant plus the device default", () => {
    const html = render(true);
    expect(html).toContain(`data-ui-bridge-id="${FLEET_PICKER_TENANT_SELECT_ID}"`);
    expect(html).toContain("device default");
    expect(html.match(/<option/g)?.length).toBe(4);
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
