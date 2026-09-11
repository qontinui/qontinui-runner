import { describe, it, expect } from "vitest";
import {
  createButtonState,
  describeRemoteCreateFailure,
  fleetDeviceCreateId,
  type RemoteCreateErrorWire,
} from "./remoteCreate";

/** Coord's real `403` body for the OFF-by-default dial, field for field. */
const coordPreferenceOff: RemoteCreateErrorWire = {
  stage: "mint",
  code: "create_forbidden:preference_off",
  message:
    "Remote terminal CREATION is OFF on the target device — `accept_remote_create` reads `off`, " +
    "which is the default until someone opts in.",
  detail: {
    error: "create_forbidden",
    reason: "preference_off",
    target_device_id: "11111111-1111-1111-1111-111111111111",
    preference: "accept_remote_create",
    preference_route: "PUT /coord/devices/me/create-preference",
    preference_default: "off",
    preference_allowed: ["off", "same_user", "tenant"],
  },
};

describe("describeRemoteCreateFailure", () => {
  /**
   * **The discriminating test.**
   *
   * `accept_remote_create` is off on every fresh device, so this refusal is
   * what EVERY first use of the button produces. The deliverable is not "it
   * failed" — it is that the operator is handed the switch. This asserts the
   * remedy names the preference, the exact route coord published, and an
   * admitting value that is not `off`; and that it says the attach dial does
   * not cover this, which is the mistake the two-dial design invites.
   *
   * Confirmed to discriminate by mutation — see the sibling test below.
   */
  it("hands the operator the switch for the off-by-default refusal", () => {
    const r = describeRemoteCreateFailure(coordPreferenceOff);

    expect(r.stage).toBe("mint");
    expect(r.code).toBe("create_forbidden:preference_off");
    expect(r.headline).toMatch(/switched off/i);
    // coord's own prose survives.
    expect(r.explanation).toContain("accept_remote_create");

    const remedy = r.remedy.join("\n");
    expect(remedy).toContain("accept_remote_create");
    // The ROUTE coord published, not one pinned in the frontend.
    expect(remedy).toContain("PUT /coord/devices/me/create-preference");
    // An admitting value is offered, and `off` is not offered as one.
    expect(remedy).toContain("same_user");
    expect(remedy).not.toMatch(/"off"/);
    // The two-dial trap, named.
    expect(remedy).toMatch(/attach.*does not enable/i);
    expect(r.strandedTerminalId).toBeNull();
  });

  /**
   * The mutation check, executable rather than described: the route and the
   * admitting values are read from coord's body, so a body carrying DIFFERENT
   * ones must produce a different remedy. A remedy hardcoded in this file
   * would pass the test above and fail this one.
   */
  it("reads the route and the admitting values from coord's body, not from a copy", () => {
    const moved = describeRemoteCreateFailure({
      ...coordPreferenceOff,
      detail: {
        ...coordPreferenceOff.detail,
        preference: "accept_remote_spawn",
        preference_route: "PUT /coord/devices/me/spawn-preference",
        preference_allowed: ["off", "fleet_wide"],
      },
    });
    const remedy = moved.remedy.join("\n");
    expect(remedy).toContain("accept_remote_spawn");
    expect(remedy).toContain("PUT /coord/devices/me/spawn-preference");
    expect(remedy).toContain("fleet_wide");
    expect(remedy).not.toContain("PUT /coord/devices/me/create-preference");
  });

  /** A body with no route still names the preference — less specific, not silent. */
  it("degrades to naming the preference when coord published no route", () => {
    const r = describeRemoteCreateFailure({
      stage: "mint",
      code: "create_forbidden:preference_off",
      message: "off",
      detail: { reason: "preference_off" },
    });
    expect(r.remedy.join("\n")).toContain("accept_remote_create");
    expect(r.remedy.length).toBeGreaterThan(0);
  });

  /**
   * The three coord refusals get three different remedies. Collapsing them
   * would send an operator to change a dial that is already correct — a
   * `same_user` refusal is not fixed by setting a preference that is already
   * set.
   */
  it("keeps the three coord refusals apart", () => {
    const off = describeRemoteCreateFailure(coordPreferenceOff);
    const user = describeRemoteCreateFailure({
      stage: "mint",
      code: "create_forbidden:different_user",
      message: "x",
    });
    const tenant = describeRemoteCreateFailure({
      stage: "mint",
      code: "create_forbidden:cross_tenant",
      message: "x",
    });
    expect(new Set([off.headline, user.headline, tenant.headline]).size).toBe(3);
    expect(user.remedy.join("\n")).toContain("same_user");
    expect(tenant.remedy.join("\n")).toMatch(/tenant/i);
  });

  /**
   * The TARGET's own local dial is a different switch in a different store
   * from coord's, and the remedy has to say so — sending someone to coord's
   * route when the runner's settings.json is what refused fixes nothing.
   */
  it("distinguishes the target's local dial from coord's column", () => {
    const r = describeRemoteCreateFailure({
      stage: "create",
      code: "remote_create_disabled",
      message: "this device does not accept remote terminal creation",
    });
    const remedy = r.remedy.join("\n");
    expect(remedy).toContain("settings.json");
    expect(remedy).toContain("remote_create.accept_remote_create");
    expect(remedy).not.toContain("PUT /coord");
  });

  /**
   * A create that succeeded and an attach that did not leaves a live PTY on
   * another machine. The panel must say so — and must point at the OTHER dial,
   * `accept_remote_attach`, rather than the one that just admitted the create.
   */
  it("reports a stranded terminal and names the attach dial", () => {
    const r = describeRemoteCreateFailure({
      stage: "attach",
      code: "attach_forbidden",
      message: "The terminal WAS created on the remote device (terminal t-9), but ...",
      detail: {
        preference: "accept_remote_attach",
        preference_route: "PUT /coord/devices/me/attach-preference",
        preference_allowed: ["off", "same_user", "tenant"],
      },
      createdTerminalId: "t-9",
    });
    expect(r.strandedTerminalId).toBe("t-9");
    expect(r.headline).toMatch(/created/i);
    const remedy = r.remedy.join("\n");
    expect(remedy).toContain("accept_remote_attach");
    expect(remedy).toContain("PUT /coord/devices/me/attach-preference");
    // The ACTIONABLE lines point at the attach dial; the create dial appears
    // only in the closing sentence that tells the two apart, never as the
    // thing to go and change.
    expect(r.remedy[0]).toContain("accept_remote_attach");
    expect(r.remedy[1]).toContain("attach-preference");
    expect(r.remedy[0]).not.toContain("accept_remote_create");
    expect(r.remedy[1]).not.toContain("create-preference");
  });

  it("reports a created-but-unaddressable terminal without inventing a fix", () => {
    const r = describeRemoteCreateFailure({
      stage: "attach",
      code: "no_coord_session",
      message: "The terminal WAS created ... but it reported no coord session id",
      createdTerminalId: "t-4",
    });
    expect(r.strandedTerminalId).toBe("t-4");
    expect(r.remedy.join("\n")).toContain("coord session id");
  });

  /**
   * An untyped rejection keeps its own text and gets NO remedy. An invented
   * remedy here would be the same defect as a manufactured UNKNOWN: it reads
   * as knowledge nobody has.
   */
  it("offers no remedy for a failure it does not recognise", () => {
    for (const raw of ["boom", new Error("boom"), 7, undefined]) {
      const r = describeRemoteCreateFailure(raw);
      expect(r.remedy).toEqual([]);
      expect(r.stage).toBe("unknown");
      expect(r.explanation.length).toBeGreaterThan(0);
    }
    const unknownCode = describeRemoteCreateFailure({
      stage: "create",
      code: "some_future_refusal",
      message: "who knows",
    });
    expect(unknownCode.remedy).toEqual([]);
    expect(unknownCode.explanation).toBe("who knows");
  });

  /** A relay-side failure says the grant is spent and a terminal may exist. */
  it("says a timed-out create may still have spawned something", () => {
    const r = describeRemoteCreateFailure({
      stage: "create",
      code: "timeout",
      message: "no remote_terminal_created within 45s",
    });
    expect(r.remedy.join("\n")).toMatch(/spent/);
    expect(r.remedy.join("\n")).toMatch(/unattached/);
  });
});

describe("createButtonState", () => {
  it("refuses the caller's own device with a reason", () => {
    const s = createButtonState({ deviceId: "d1", isCallerDevice: true });
    expect(s.disabled).toBe(true);
    expect(s.reason).toMatch(/this machine/i);
  });

  it("refuses a group with no device id", () => {
    expect(createButtonState({ deviceId: "  ", isCallerDevice: false }).disabled).toBe(true);
  });

  it("admits a remote device", () => {
    expect(createButtonState({ deviceId: "d2", isCallerDevice: false })).toEqual({
      disabled: false,
      reason: null,
    });
  });
});

describe("fleetDeviceCreateId", () => {
  it("is per-device so two groups are not one control", () => {
    expect(fleetDeviceCreateId("a")).not.toBe(fleetDeviceCreateId("b"));
  });
});
