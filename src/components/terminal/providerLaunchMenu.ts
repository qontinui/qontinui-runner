/**
 * Pure helpers for the Terminal page's provider launch menu (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
 * Phase 6). The menu lists EVERY served CLI profile — never a hard-coded
 * list — each with the verdict of its availability probe
 * (`cli_profile_availability`, `src-tauri/src/commands/cli_availability.rs`):
 *
 * - available → launchable, with the version it reported;
 * - absent → listed DISABLED with the reason and this OS's install command,
 *   never hidden;
 * - UNKNOWN (the binary was found but its probe failed, or the probe itself
 *   could not run) → launchable, and labelled UNKNOWN rather than guessed.
 *
 * Every entry's DEFAULT launch is a PTY-hosted session: the menu types the
 * CLI's command into a terminal. A second, explicit per-launch choice — a
 * structured session (plan Phase 9) — is offered only for a profile whose
 * structured lane is one the RUNNER implements (Claude stream-json; Codex's
 * `codex app-server` is declared by its profile but not spoken by the runner),
 * whose typed permission requests are verified, and which names the arguments
 * that route them to the runner ({@link structuredLaunchOffer}). A structured
 * session asks before each tool call through a permission card.
 *
 * Kept free of React and Tauri so the runner's node-environment vitest can
 * cover it (`providerLaunchMenu.test.ts`).
 */

/** The fields of a served `CliProfile` the menu reads. */
export interface LaunchMenuProfile {
  id: string;
  displayName: string;
  install?: { linux?: string; macos?: string; windows?: string };
  /** The CLI's structured protocol (`{ kind: "claude_stream_json" }`, …). */
  structuredLane?: { kind: string };
  /** `"supported" | "unsupported" | "unknown"` (absent reads as unknown). */
  typedPermission?: string;
  /** The arguments that route permission requests to the runner. */
  permissionPromptArgs?: string[];
}

/** The structured lanes the RUNNER speaks. Codex's app-server is follow-up A. */
export const RUNNER_IMPLEMENTED_STRUCTURED_LANES: readonly string[] = ["claude_stream_json"];

/** Whether a profile gets the "structured session" launch choice, and why not. */
export type StructuredLaunchOffer = { offered: true } | { offered: false; reason: string };

/**
 * The structured-launch verdict for one profile. Pure. Mirrors the runner's
 * own refusal (`structured_launch_profile`, `commands/structured_session.rs`),
 * which stays the authority — this only decides whether to show the button.
 */
export function structuredLaunchOffer(profile: LaunchMenuProfile): StructuredLaunchOffer {
  const lane = profile.structuredLane?.kind ?? "unknown";
  if (!RUNNER_IMPLEMENTED_STRUCTURED_LANES.includes(lane)) {
    return {
      offered: false,
      reason: `no structured lane this runner implements (${lane})`,
    };
  }
  if (profile.typedPermission !== "supported") {
    return {
      offered: false,
      reason: `typed permission requests are ${profile.typedPermission ?? "unknown"}`,
    };
  }
  if (!profile.permissionPromptArgs || profile.permissionPromptArgs.length === 0) {
    return { offered: false, reason: "no argument routes permission requests to the runner" };
  }
  return { offered: true };
}

/** Wire shape of `cli_profile_availability` (Rust `CliAvailability`). */
export interface CliAvailability {
  id: string;
  /** `true` ran, `false` not on PATH, `null` UNKNOWN. */
  available: boolean | null;
  version?: string | null;
  error?: string | null;
}

/** Which install command applies. */
export type InstallOs = "linux" | "macos" | "windows";

/** One row of the menu. */
export interface LaunchEntry {
  id: string;
  label: string;
  state: "available" | "absent" | "unknown" | "probing";
  /** Whether the Launch control is enabled. */
  enabled: boolean;
  /** The version, the reason, or the probe's error — what to show beside the label. */
  detail: string | null;
  /** This OS's install command, shown for an absent CLI only. */
  installCommand: string | null;
}

/** The install-command key for a `navigator.platform` string. */
export function installOsFor(platform: string): InstallOs {
  const p = platform.toLowerCase();
  if (p.startsWith("win")) return "windows";
  if (p.startsWith("mac")) return "macos";
  return "linux";
}

/**
 * The menu row for one profile. `availability` is `undefined` while its probe
 * is in flight and `null` when the probe could not be made at all (the Tauri
 * command rejected) — the latter is UNKNOWN, like a probe that ran and failed.
 */
export function launchEntry(
  profile: LaunchMenuProfile,
  availability: CliAvailability | null | undefined,
  os: InstallOs,
  probeError?: string,
): LaunchEntry {
  const base = { id: profile.id, label: profile.displayName };
  if (availability === undefined) {
    return { ...base, state: "probing", enabled: false, detail: "checking…", installCommand: null };
  }
  if (availability === null || availability.available === null) {
    return {
      ...base,
      state: "unknown",
      enabled: true,
      detail: `UNKNOWN — ${availability?.error ?? probeError ?? "the availability probe did not run"}`,
      installCommand: null,
    };
  }
  if (availability.available) {
    return {
      ...base,
      state: "available",
      enabled: true,
      detail: availability.version ?? null,
      installCommand: null,
    };
  }
  return {
    ...base,
    state: "absent",
    enabled: false,
    detail: availability.error ?? "not installed",
    installCommand: profile.install?.[os] ?? null,
  };
}
