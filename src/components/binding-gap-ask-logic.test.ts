import { describe, it, expect } from "vitest";
import {
  isAlreadyInProgress,
  normalizePairAllStatus,
  bannerGapEntries,
  displayNameFor,
  normalizePairAllProgress,
  credentialStateLabel,
  dismissKey,
  gapsCleared,
  normalizeBindingGapAsks,
  normalizeBindingGapView,
  normalizePairAllResults,
  pairResultLabel,
  rowsNeedingConnect,
  type BindingGapAsk,
} from "./binding-gap-ask-logic";

const T = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

const raw = {
  tenant_id: T,
  detector: "binding_gaps",
  reason: "no headless path can seed this tenant's credential safely",
  message: `This device is bound to tenant ${T} but holds no credential for it.`,
  cta: "device_pair_tenant",
  command: `qontinui_profile device pair --tenant-id ${T}`,
  caveat: "pairing also makes this tenant the device's home tenant",
  first_seen: 1800000000,
};

describe("normalizeBindingGapAsks", () => {
  it("maps the refresher's payload", () => {
    const asks = normalizeBindingGapAsks([raw]);
    expect(asks).toEqual([
      {
        tenantId: T,
        command: raw.command,
        message: raw.message,
        reason: raw.reason,
        caveat: raw.caveat,
        firstSeen: 1800000000,
      },
    ]);
  });

  it("treats a missing or non-array answer as UNKNOWN, never as no asks", () => {
    expect(normalizeBindingGapAsks(null)).toBeNull();
    expect(normalizeBindingGapAsks(undefined)).toBeNull();
    expect(normalizeBindingGapAsks({})).toBeNull();
    expect(normalizeBindingGapAsks([])).toEqual([]);
  });

  it("drops entries the operator could not act on", () => {
    expect(normalizeBindingGapAsks([{ tenant_id: T }, { command: "x" }, 7, null])).toEqual([]);
  });
});

describe("dismissKey", () => {
  it("keys on tenant AND lapse, so a new lapse is shown again", () => {
    const ask = normalizeBindingGapAsks([raw])![0] as BindingGapAsk;
    expect(dismissKey(ask)).toBe(`${T}@1800000000`);
    expect(dismissKey({ ...ask, firstSeen: null })).toBe(`${T}@unknown`);
  });
});

const A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

const measured = {
  status: "measured",
  reason: null,
  rows: [
    { tenant_id: A, display_name: null, state: "connected" },
    { tenant_id: T, display_name: "Acme", state: "no_credential" },
  ],
};

describe("normalizeBindingGapView", () => {
  it("maps a measured view", () => {
    expect(normalizeBindingGapView(measured)).toEqual({
      status: "measured",
      reason: null,
      rows: [
        { tenantId: A, displayName: null, state: "connected" },
        { tenantId: T, displayName: "Acme", state: "no_credential" },
      ],
    });
  });

  it("renders an UNKNOWN view with every row unknown — never connected", () => {
    const view = normalizeBindingGapView({
      status: "unknown",
      reason: "coord_bound_tenants.json is stale",
      // Even a row that CLAIMS connected is not trusted under an unknown view.
      rows: [{ tenant_id: A, display_name: null, state: "connected" }],
    });
    expect(view?.status).toBe("unknown");
    expect(view?.reason).toBe("coord_bound_tenants.json is stale");
    expect(view?.rows.map((r) => r.state)).toEqual(["unknown"]);
    expect(view?.rows.map((r) => credentialStateLabel(r.state))).toEqual(["unknown"]);
  });

  it("treats an unrecognised status or state as unknown", () => {
    expect(normalizeBindingGapView({ status: "fine", rows: [] })?.status).toBe("unknown");
    const view = normalizeBindingGapView({
      status: "measured",
      rows: [{ tenant_id: A, state: "healthy" }],
    });
    expect(view?.rows[0]?.state).toBe("unknown");
  });

  it("returns null (UNKNOWN) for a missing or malformed answer", () => {
    expect(normalizeBindingGapView(null)).toBeNull();
    expect(normalizeBindingGapView(undefined)).toBeNull();
    expect(normalizeBindingGapView([])).toBeNull();
    expect(normalizeBindingGapView({ status: "measured" })).toBeNull();
  });

  it("labels each state for the Settings rows", () => {
    expect(credentialStateLabel("connected")).toBe("connected");
    expect(credentialStateLabel("no_credential")).toBe("no credential");
    expect(credentialStateLabel("unknown")).toBe("unknown");
  });
});

describe("rowsNeedingConnect / gapsCleared", () => {
  it("offers ONLY no_credential rows — never unknown, never connected", () => {
    const view = normalizeBindingGapView({
      status: "measured",
      rows: [
        { tenant_id: A, state: "connected" },
        { tenant_id: B, state: "unknown" },
        { tenant_id: T, state: "no_credential" },
      ],
    });
    expect(rowsNeedingConnect(view).map((r) => r.tenantId)).toEqual([T]);
    expect(rowsNeedingConnect(null)).toEqual([]);
  });

  it("offers nothing on an UNKNOWN view", () => {
    const view = normalizeBindingGapView({
      status: "unknown",
      reason: "coord_bound_tenants.json is stale",
      rows: [{ tenant_id: T, state: "no_credential" }],
    });
    expect(rowsNeedingConnect(view)).toEqual([]);
  });

  it("is cleared only on a MEASURED view with no gap", () => {
    expect(gapsCleared(normalizeBindingGapView(measured))).toBe(false);
    expect(
      gapsCleared(
        normalizeBindingGapView({
          status: "measured",
          rows: [{ tenant_id: A, state: "connected" }],
        }),
      ),
    ).toBe(true);
    expect(gapsCleared(normalizeBindingGapView({ status: "unknown", rows: [] }))).toBe(false);
    expect(gapsCleared(null)).toBe(false);
  });
});

describe("bannerGapEntries", () => {
  const view = normalizeBindingGapView(measured);
  const asks = normalizeBindingGapAsks([raw]);

  it("lists the view's gaps, keyed by the ask's lapse, with its terminal fallback", () => {
    expect(bannerGapEntries(view, asks, new Set())).toEqual([
      {
        tenantId: T,
        displayName: "Acme",
        key: `${T}@1800000000`,
        command: raw.command,
        caveat: raw.caveat,
      },
    ]);
  });

  it("derives from the view, not the asks: an ask for a now-connected tenant shows nothing", () => {
    const healed = normalizeBindingGapView({
      status: "measured",
      rows: [{ tenant_id: T, state: "connected" }],
    });
    expect(bannerGapEntries(healed, asks, new Set())).toEqual([]);
  });

  it("shows a gap with no recorded ask yet, with a built command", () => {
    const [entry] = bannerGapEntries(view, [], new Set());
    expect(entry?.key).toBe(`${T}@unrecorded`);
    expect(entry?.command).toBe(`qontinui_profile device pair --tenant-id ${T}`);
  });

  it("honours dismissal, and shows nothing when UNKNOWN or signed out", () => {
    expect(bannerGapEntries(view, asks, new Set([`${T}@1800000000`]))).toEqual([]);
    expect(
      bannerGapEntries(normalizeBindingGapView({ status: "unknown", rows: [] }), asks, new Set()),
    ).toEqual([]);
    expect(bannerGapEntries(view, null, new Set())).toEqual([]);
    expect(bannerGapEntries(null, asks, new Set())).toEqual([]);
  });
});

describe("normalizePairAllResults", () => {
  it("maps rows and labels them", () => {
    const rows = normalizePairAllResults({
      results: [
        { tenant_id: A, status: "connected", skipped_reason: null },
        { tenant_id: B, status: "skipped", skipped_reason: "not_a_member" },
        { tenant_id: T, status: "weird", skipped_reason: "disk full" },
      ],
    });
    expect(rows?.map((r) => r.status)).toEqual(["connected", "skipped", "failed"]);
    expect(rows?.map(pairResultLabel)).toEqual([
      "connected",
      "skipped — you are not a member of this workspace",
      "failed — disk full",
    ]);
    expect(normalizePairAllResults(null)).toBeNull();
    expect(normalizePairAllResults({})).toBeNull();
  });
});

describe("displayNameFor", () => {
  it("takes the name from the view, else null", () => {
    const view = normalizeBindingGapView(measured);
    expect(displayNameFor(view, T)).toBe("Acme");
    expect(displayNameFor(view, A)).toBeNull();
    expect(displayNameFor(null, T)).toBeNull();
  });
});

describe("normalizePairAllProgress", () => {
  it("maps every phase", () => {
    expect(normalizePairAllProgress({ phase: "waiting", tenant_ids: [T] })).toEqual({
      phase: "waiting",
    });
    expect(
      normalizePairAllProgress({ phase: "browser", connect_url: "https://x/c", launched: false }),
    ).toEqual({ phase: "browser", connectUrl: "https://x/c", launched: false });
    expect(
      normalizePairAllProgress({
        phase: "done",
        results: [{ tenant_id: T, status: "connected" }],
      }),
    ).toEqual({
      phase: "done",
      results: [{ tenantId: T, status: "connected", skippedReason: null }],
    });
    expect(normalizePairAllProgress({ phase: "error", error: "boom" })).toEqual({
      phase: "error",
      error: "boom",
    });
    expect(normalizePairAllProgress({ phase: "cancelled" })).toEqual({ phase: "cancelled" });
  });

  it("rejects malformed payloads", () => {
    expect(normalizePairAllProgress(null)).toBeNull();
    expect(normalizePairAllProgress({ phase: "browser" })).toBeNull();
    expect(normalizePairAllProgress({ phase: "nope" })).toBeNull();
  });
});

describe("normalizePairAllStatus / isAlreadyInProgress", () => {
  it("maps an in-flight status and idles a finished one", () => {
    expect(
      normalizePairAllStatus({
        in_flight: true,
        phase: "browser",
        connect_url: "https://x/c",
        launched: false,
      }),
    ).toEqual({ inFlight: true, phase: "browser", connectUrl: "https://x/c", launched: false });
    expect(normalizePairAllStatus({ in_flight: false, phase: "browser" })?.phase).toBe("idle");
    expect(normalizePairAllStatus({ in_flight: true, phase: "collecting" })?.phase).toBe(
      "collecting",
    );
    expect(normalizePairAllStatus({})).toBeNull();
    expect(normalizePairAllStatus(null)).toBeNull();
  });

  it("recognises the runner's one-flow-at-a-time refusal", () => {
    expect(
      isAlreadyInProgress(
        "a workspace sign-in is already in progress — finish it in your browser first",
      ),
    ).toBe(true);
    expect(isAlreadyInProgress("cancelled")).toBe(false);
  });

  it("maps the collecting phase of the progress event", () => {
    expect(normalizePairAllProgress({ phase: "collecting" })).toEqual({ phase: "collecting" });
  });
});
