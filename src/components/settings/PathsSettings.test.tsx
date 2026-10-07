/**
 * The Paths panel's capture copy (plan
 * 2026-09-22-plans-dir-is-a-single-path-so-a-multi-bound-device-cannot-author-per-tenant,
 * P6 gate disclosure, #1842).
 *
 * Authoring is per tenant but scanning is device-wide until P6 lands, so the
 * per-tenant block must say a per-tenant directory is not scanned — and must
 * say it ONLY while the tier is on. With the tier off nothing is scanned at all,
 * and the "device-wide only" reassurance would contradict the amber banner.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the
 * components are rendered with `renderToStaticMarkup`.
 */

import { describe, it, expect, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@/contexts/TenantContext", () => ({
  useTenant: () => ({ defaultTenantIdForNewSessions: null, candidates: [], showSwitcher: false }),
}));

import { PerTenantPaths, PlanScanStatus } from "./PathsSettings";
import { tenantDraftsFrom, type PathSettings, type ResolvedPaths } from "./pathsSettingsHelpers";

const TIER_ON_NOTE = "Sessions write per tenant; the scanner reads device-wide.";
const TIER_OFF_NOTE = "Per-tenant paths do not turn scanning on.";

const configured: PathSettings = { plans_dir: "/plans", strict_mode: false };

function perTenant(planTierActive: boolean, showSwitcher = true): string {
  return renderToStaticMarkup(
    <PerTenantPaths
      showSwitcher={showSwitcher}
      candidates={["tenant-a", "tenant-b"]}
      defaultTenantId="tenant-a"
      planTierActive={planTierActive}
      configured={configured}
      drafts={tenantDraftsFrom(configured)}
      onChange={() => {}}
      onBrowse={() => {}}
    />,
  );
}

function resolved(planTierActive: boolean): ResolvedPaths {
  return {
    plans_dir: planTierActive ? "/plans" : null,
    prompts_dir: null,
    workspace_root: null,
    dev_logs_dir: "/logs",
    plan_tier_active: planTierActive,
    plan_scan_roots: planTierActive ? 1 : null,
    plan_scan_divergence: null,
  };
}

describe("PerTenantPaths capture note", () => {
  it("says per-tenant directories are not scanned while the tier is on", () => {
    const html = perTenant(true);
    expect(html).toContain(TIER_ON_NOTE);
    expect(html).toContain("is not scanned unless that directory is also the device-wide one");
    expect(html).not.toContain(TIER_OFF_NOTE);
  });

  it("says nothing is scanned at all while the tier is off", () => {
    const html = perTenant(false);
    expect(html).toContain(TIER_OFF_NOTE);
    expect(html).not.toContain(TIER_ON_NOTE);
  });

  it("renders neither note when the per-tenant rows are hidden", () => {
    for (const tier of [true, false]) {
      const html = perTenant(tier, false);
      expect(html).not.toContain(TIER_ON_NOTE);
      expect(html).not.toContain(TIER_OFF_NOTE);
    }
  });
});

describe("PlanScanStatus banner", () => {
  it("names the device-wide directory in both states", () => {
    expect(renderToStaticMarkup(<PlanScanStatus resolved={resolved(true)} />)).toContain(
      "scanning the device-wide plans directory",
    );
    expect(renderToStaticMarkup(<PlanScanStatus resolved={resolved(false)} />)).toContain(
      "No device-wide plans directory is in effect",
    );
  });
});
