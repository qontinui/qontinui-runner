/**
 * Pure helpers for `RemoteCreateSettings` — kept out of the component so the
 * runner's node-environment vitest can exercise them without a DOM.
 *
 * `accept_remote_create` decides whether OTHER machines in the tenant may
 * SPAWN a new terminal session on this one (plan
 * `2026-09-11-headless-runner-parity-from-a-headed-runner`, D2). It is a
 * separate dial from `accept_remote_attach` and defaults to `off`, because a
 * create spawns a PTY (and may allocate a worktree / take a coord claim) where
 * an attach only drives one the operator already opened.
 */

export type AcceptRemoteCreate = "off" | "same_user" | "tenant";

/** Mirrors `AcceptRemoteCreate::default()` in `src-tauri/src/settings.rs`. */
export const ACCEPT_REMOTE_CREATE_DEFAULT: AcceptRemoteCreate = "off";

export const ACCEPT_REMOTE_CREATE_OPTIONS: ReadonlyArray<{
  value: AcceptRemoteCreate;
  label: string;
  /** One line, no hedging: what this value lets happen. */
  explain: string;
}> = [
  {
    value: "off",
    label: "Off",
    explain:
      "No machine may start a session here remotely; coord refuses to mint a create grant — the default.",
  },
  {
    value: "same_user",
    label: "Same user",
    explain: "Only devices signed in as the same user as this one may start a session here.",
  },
  {
    value: "tenant",
    label: "Whole tenant",
    explain:
      "Any device in this tenant may start a session here, still one coord-minted grant each.",
  },
];

export function isAcceptRemoteCreate(raw: unknown): raw is AcceptRemoteCreate {
  return raw === "off" || raw === "same_user" || raw === "tenant";
}

/**
 * The `coord_mirror` object `remote_create_preference_*` returns
 * (`MirrorOutcome::to_json` in `src-tauri/src/commands/remote_create.rs`).
 */
export interface CoordMirror {
  state: "agreed" | "diverged" | "not_stored" | "unreachable" | "unknown";
  consistent: boolean;
  value?: string;
  local?: string;
  coord?: string;
  detail?: string;
}

export type MirrorTone = "ok" | "warn" | "unknown";

/**
 * One line describing whether coord's copy of the dial matches this runner's.
 * Only `agreed` is reported as consistency; every other state — including
 * "no mirror has run yet" — is surfaced as what it is, never as agreement.
 */
export function describeCoordMirror(mirror: CoordMirror | null | undefined): {
  tone: MirrorTone;
  text: string;
} {
  if (!mirror || typeof mirror !== "object") {
    return { tone: "unknown", text: "Coord mirror state unknown." };
  }
  switch (mirror.state) {
    case "agreed":
      return { tone: "ok", text: `Coord agrees (${mirror.value ?? "?"}), verified by read-back.` };
    case "diverged":
      return {
        tone: "warn",
        text: `Coord reads \`${mirror.coord ?? "?"}\` while this runner enforces \`${mirror.local ?? "?"}\`. A create needs BOTH to admit it (coord's mint reads coord's value, this runner reads its own), so until they agree the narrower one governs; retried on the next relay connect.`,
      };
    case "not_stored":
      return {
        tone: "warn",
        text: `Coord holds no stored value for this device, so its mint reads the default (off) and refuses every remote create regardless of the value here${mirror.detail ? ` — ${mirror.detail}` : "."}`,
      };
    case "unreachable":
      return {
        tone: "warn",
        text: `Coord mirror failed${mirror.detail ? ` (${mirror.detail})` : ""}; retried on the next relay connect.`,
      };
    default:
      return {
        tone: "unknown",
        text: "No coord mirror has run in this process yet — use “Check coord” to verify.",
      };
  }
}
