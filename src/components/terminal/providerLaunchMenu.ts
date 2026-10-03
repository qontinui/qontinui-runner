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
 * Every entry launches a PTY-hosted session: the menu types the CLI's command
 * into a terminal. It never offers a structured lane — a profile may declare
 * one as a fact about its CLI (Codex `codex app-server`), but the runner
 * implements none outside Claude's stream-json workers.
 *
 * Kept free of React and Tauri so the runner's node-environment vitest can
 * cover it (`providerLaunchMenu.test.ts`).
 */

/** The fields of a served `CliProfile` the menu reads. */
export interface LaunchMenuProfile {
  id: string;
  displayName: string;
  install?: { linux?: string; macos?: string; windows?: string };
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
