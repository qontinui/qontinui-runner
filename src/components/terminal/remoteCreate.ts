/**
 * Remote terminal CREATION — the source-side view of `terminal_create_remote`
 * (plan `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase 5).
 *
 * Pure helpers only (the runner's vitest config is `environment: "node"`), so
 * the thing that actually matters here — how a REFUSAL renders — is unit-tested
 * rather than only visible in a running window.
 *
 * ## Why the refusal is the centrepiece of this module
 *
 * `accept_remote_create` is `off` on every device until someone opts in, and it
 * is a SEPARATE consent from `accept_remote_attach`: enabling remote attach
 * does not enable remote create, because a create lets a remote peer start a
 * session that allocates worktrees and takes coord claims on the target
 * machine. So the first thing a new user of this button meets is a refusal —
 * every time, by design — and a refusal rendered as one red line reads as a
 * broken feature. Both refusing parties already publish the switch: coord's
 * `403` body carries `preference`, `preference_route`, `preference_allowed` and
 * a prose `hint`; the target's own gate names its local settings key. This
 * module's job is to get that to the operator intact, and to say plainly when
 * it is NOT the create dial that refused.
 */

/** The object `terminal_create_remote` rejects with (Rust `RemoteCreateError`). */
export interface RemoteCreateErrorWire {
  stage?: string;
  code?: string;
  message?: string;
  detail?: Record<string, unknown> | null;
  createdTerminalId?: string | null;
}

export type RemoteCreateStage = "mint" | "create" | "attach" | "unknown";

/** What the picker renders for a failed create. */
export interface RemoteCreateRefusal {
  /** The machine code, or `""` when the failure carried none. */
  code: string;
  stage: RemoteCreateStage;
  /** One short line naming WHAT refused. */
  headline: string;
  /** The refusing party's own words, kept rather than paraphrased. */
  explanation: string;
  /**
   * Concrete, ordered steps. EMPTY when we genuinely do not know what to do —
   * an invented remedy is worse than none, because it sends an operator to
   * change a setting that was never the problem.
   */
  remedy: string[];
  /**
   * Set when a terminal WAS spawned on the target and this window is not
   * showing it. The operator has a live PTY on another machine; saying so is
   * not optional.
   */
  strandedTerminalId: string | null;
}

export function fleetDeviceCreateId(deviceId: string): string {
  return `terminal.fleet-device-create.${deviceId}`;
}

export function fleetDeviceCreateErrorId(deviceId: string): string {
  return `terminal.fleet-device-create-error.${deviceId}`;
}

function asStage(raw: unknown): RemoteCreateStage {
  return raw === "mint" || raw === "create" || raw === "attach" ? raw : "unknown";
}

function str(detail: Record<string, unknown> | null | undefined, key: string): string | null {
  const v = detail?.[key];
  return typeof v === "string" && v.trim() ? v.trim() : null;
}

function list(detail: Record<string, unknown> | null | undefined, key: string): string[] {
  const v = detail?.[key];
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/**
 * The remedy for a coord-minted-refusal whose reason is "the dial is off".
 *
 * Built from coord's OWN body where it has one — the route and the admitting
 * values move with coord, and a copy pinned in this file would be the stale
 * half of the pair the day either changes. Falls back to naming the preference
 * alone, which is still actionable and is honestly less specific.
 */
function preferenceOffRemedy(
  detail: Record<string, unknown> | null | undefined,
  preferenceFallback: string,
): string[] {
  const preference = str(detail, "preference") ?? preferenceFallback;
  const route = str(detail, "preference_route");
  const allowed = list(detail, "preference_allowed").filter((v) => v !== "off");
  const steps: string[] = [];
  steps.push(
    `The target device's \`${preference}\` preference is \`off\` — that is the default, not a fault.`,
  );
  if (route) {
    steps.push(
      allowed.length > 0
        ? `Call \`${route}\` FROM THAT DEVICE with {"${preference}": "${allowed[0]}"}` +
            (allowed.length > 1 ? ` (or "${allowed.slice(1).join('" / "')}")` : "") +
            `.`
        : `Call \`${route}\` from that device to change it.`,
    );
  } else {
    steps.push(
      `Set \`${preference}\` on that device — the runner writes it to settings and mirrors it to coord.`,
    );
  }
  return steps;
}

/**
 * Turn whatever `terminal_create_remote` rejected with into the panel the
 * picker shows.
 *
 * Never throws, and never invents: an untyped rejection keeps its own text and
 * gets no remedy list at all.
 */
export function describeRemoteCreateFailure(err: unknown): RemoteCreateRefusal {
  if (err === null || typeof err !== "object") {
    const raw = err instanceof Error ? err.message : typeof err === "string" ? err : String(err);
    return {
      code: "",
      stage: "unknown",
      headline: "Remote create failed",
      explanation: raw.trim() || "the runner gave no reason",
      remedy: [],
      strandedTerminalId: null,
    };
  }
  const wire = err as RemoteCreateErrorWire;
  const stage = asStage(wire.stage);
  const code = typeof wire.code === "string" ? wire.code : "";
  const detail = (wire.detail ?? null) as Record<string, unknown> | null;
  const explanation =
    typeof wire.message === "string" && wire.message.trim()
      ? wire.message.trim()
      : "the runner gave no reason";
  const strandedTerminalId =
    typeof wire.createdTerminalId === "string" && wire.createdTerminalId.trim()
      ? wire.createdTerminalId.trim()
      : null;

  const base = { code, stage, explanation, strandedTerminalId };

  // --- coord would not mint -------------------------------------------------
  if (code === "create_forbidden:preference_off") {
    return {
      ...base,
      headline: "Remote terminal creation is switched off on that device",
      remedy: [
        ...preferenceOffRemedy(detail, "accept_remote_create"),
        "Enabling remote ATTACH does not enable remote create — they are separate consents.",
      ],
    };
  }
  if (code === "create_forbidden:different_user") {
    return {
      ...base,
      headline: "That device only accepts remote creates from its own user",
      remedy: [
        "Its `accept_remote_create` reads `same_user`, and this device is not paired to that device's user.",
        "Either run this from a device paired to the same user, or widen that device's dial to `tenant`.",
      ],
    };
  }
  if (code === "create_forbidden:cross_tenant") {
    return {
      ...base,
      headline: "That device is in a different tenant",
      remedy: [
        "No preference value on the target could admit this call; nothing was minted.",
        "Check which tenant this runner is bound to before retrying.",
      ],
    };
  }
  if (stage === "mint" && code.startsWith("create_forbidden")) {
    return { ...base, headline: "coord refused to mint a create grant", remedy: [] };
  }
  if (code === "device_not_found") {
    return {
      ...base,
      headline: "coord has no row for that device",
      remedy: ["Refresh the fleet list — the device id this action used may be stale."],
    };
  }
  if (code === "coord_unreachable" || code === "coord_client_unavailable") {
    return {
      ...base,
      headline: "coord did not answer",
      remedy: [
        "This is a reachability answer, not a policy one — nothing about the target device is known from it.",
        "Retry once coord is reachable.",
      ],
    };
  }

  // --- the TARGET's own gate refused ---------------------------------------
  if (code === "remote_create_disabled") {
    return {
      ...base,
      headline: "The target runner itself refuses remote creates",
      remedy: [
        "coord minted the grant, and the target's OWN dial still said no — the runner enforces its local setting.",
        "Set `remote_create.accept_remote_create` to `same_user` or `tenant` in that runner's settings.json.",
        "The two stores are mirrored, so a disagreement here usually means the mirror has not run since the change.",
      ],
    };
  }
  if (code === "remote_create_no_target_directory") {
    return {
      ...base,
      headline: "The target offers nowhere to spawn a terminal",
      remedy: [
        "It resolved no working directory it is willing to use, so it refused rather than falling back to its process cwd.",
        "List one in `remote_create.allowed_working_dirs`, or set `paths.workspace_root`, on that device.",
      ],
    };
  }
  if (code === "remote_create_working_dir_not_allowed") {
    const keys = list(detail, "allowed_working_dir_keys");
    return {
      ...base,
      headline: "The target does not offer that working directory",
      remedy:
        keys.length > 0
          ? [`It offers: ${keys.join(", ")}.`]
          : [
              "It answers only with directories it has listed itself; the caller never supplies a path.",
            ],
    };
  }
  if (code.startsWith("remote_create_grant_")) {
    return {
      ...base,
      headline: "The target did not recognise the grant",
      remedy: [
        "Create grants are single-use and short-lived, and the target checks them against coord's own directive rather than the relay's word.",
        "Try again — a retry mints a fresh grant.",
      ],
    };
  }
  if (code === "timeout" || code === "relay_unavailable" || code === "relay_disconnected") {
    return {
      ...base,
      headline: "The create did not complete over the relay",
      remedy: [
        "The grant is spent whether or not the target spawned a terminal; a retry mints a new one.",
        "If a terminal WAS spawned it is running unattached on that machine — check its terminal list before retrying.",
      ],
    };
  }

  // --- created, but this window could not get into it -----------------------
  if (stage === "attach") {
    if (code.startsWith("attach_forbidden")) {
      return {
        ...base,
        headline: "The terminal was created, but remote ATTACH is off on that device",
        remedy: [
          ...preferenceOffRemedy(detail, "accept_remote_attach"),
          "`accept_remote_create` and `accept_remote_attach` are separate dials; this one lets you drive what the other let you spawn.",
        ],
      };
    }
    if (code === "no_coord_session") {
      return {
        ...base,
        headline: "The terminal was created, but it is not addressable",
        remedy: [
          "The target reported no coord session id, so no attach grant can be minted for the terminal it spawned.",
          "Its runner build may predate remote-create session registration; that runner's log says which.",
        ],
      };
    }
    return {
      ...base,
      headline: "The terminal was created, but the tab could not be opened",
      remedy: [],
    };
  }

  return { ...base, headline: "Remote create failed", remedy: [] };
}

/** Per-device create state, kept inline in the picker rather than in a toast. */
export interface DeviceCreateState {
  pending: boolean;
  refusal: RemoteCreateRefusal | null;
  /** Local terminal id of the tab the last successful create opened. */
  openedId: string | null;
}

export const IDLE_DEVICE_CREATE: DeviceCreateState = {
  pending: false,
  refusal: null,
  openedId: null,
};

/**
 * Whether the "New terminal" action may fire for a device group, and why not.
 *
 * The caller's OWN device is excluded: creating a terminal here is the ordinary
 * local button, and routing it through coord + the relay to come back to this
 * same process would be a worse version of it.
 */
export function createButtonState(group: { deviceId: string; isCallerDevice: boolean }): {
  disabled: boolean;
  reason: string | null;
} {
  if (group.isCallerDevice) {
    return {
      disabled: true,
      reason: "This is this machine — use the ordinary New Terminal button.",
    };
  }
  if (!group.deviceId?.trim()) {
    return { disabled: true, reason: "coord did not report a device id to address." };
  }
  return { disabled: false, reason: null };
}
