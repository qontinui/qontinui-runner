/**
 * Per-provider session facts for the boot-restore UX, read from the runner's
 * served CLI profiles (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
 * Phase 4).
 *
 * The frontend holds NO provider data of its own. The single source is the
 * Rust manifest (`src-tauri/src/cli_profile/`), fetched once through the Tauri
 * command `terminal_cli_profiles` (the same data `GET /terminals/cli-profiles`
 * serves a headless runner or remote webview) and cached here. Each profile is
 * exposed as a {@link SessionProviderDescriptor}: the resume command, the
 * resume handshake patterns `resumeVerification` matches against, and the
 * restore tier.
 *
 * **An unknown provider has no descriptor — and neither does any provider
 * before the cache is primed.** {@link providerDescriptorFor} answers `null`,
 * never a default CLI's descriptor, so a record of a provider the runner does
 * not know (or cannot yet vouch for) restores terminal-only instead of having
 * another CLI's resume typed at it. Synchronous callers must therefore run after
 * {@link loadCliProfiles} has resolved; the restore path awaits it before
 * classifying any record.
 */

import { invoke } from "@tauri-apps/api/core";

/**
 * Declared restore capability of a provider.
 * - `"full"`: the provider deterministically resumes the FULL conversation by
 *   id — restore brings the chat back.
 * - `"terminal-only"`: only terminal+cwd+launch-command restore; no
 *   conversation resume. The UI is honest about the loss ("fresh conversation").
 */
export type RestoreTier = "full" | "terminal-only";

/**
 * Success/failure markers for `resumeVerification`, which checks failure
 * first and unions both kinds. Failure markers match ANSI-stripped text;
 * success markers match rendered text, where cursor motion becomes whitespace
 * (`renderAnsi`), because Claude Code v2 draws word gaps with cursor moves:
 *
 * - `success` / `failure` — case-insensitive SUBSTRINGS.
 * - `successPatterns` / `failurePatterns` — REGEXES, for markers a substring
 *   cannot express (the Claude TUI's input-box frames are shapes, not
 *   phrases).
 * - `titlePatterns` — REGEXES matched against the pane's current window title
 *   only (the profile's `titleRegex`).
 *
 * Every regex is compiled from the profile's sources with the `"i"` flag:
 *   the manifest's dialect contract is that every source matches
 *   case-insensitively in both engines and carries no inline flags
 *   (`src-tauri/src/cli_profile/mod.rs`).
 */
export interface HandshakePatterns {
  /** Substrings whose presence confirms the resume landed. */
  success: string[];
  /** Substrings whose presence means the resume FAILED (drives the banner). */
  failure: string[];
  /** Regex markers unioned with {@link HandshakePatterns.success}. */
  successPatterns?: RegExp[];
  /** Regex markers unioned with {@link HandshakePatterns.failure}. */
  failurePatterns?: RegExp[];
  /**
   * Regexes matched against the pane's CURRENT window title only, never its
   * body. A title the provider sets at launch is evidence the body cannot
   * fake, provided the pattern is anchored to the provider's own title.
   */
  titlePatterns?: RegExp[];
}

/** The capability surface the boot-restore UX needs from one provider. */
export interface SessionProviderDescriptor {
  /** Provider id (`"claude"`). Matches a session record's `provider`. */
  provider: string;
  /**
   * The deterministic, non-interactive resume argv for `sessionId`, or `null`
   * when the provider declares no resume by id.
   */
  resumeCommand(sessionId: string): string[] | null;
  /** Resume success/failure handshake patterns. */
  handshakePatterns(): HandshakePatterns;
  /** Declared restore capability for the honest-UX surface. */
  restoreTier(): RestoreTier;
}

/**
 * The fields of the served `CliProfile` (`qontinui-schemas`
 * `rust/src/cli_session.rs`) this module reads. `@qontinui/shared-types`
 * 0.6.0, the version the runner installs from npm, predates that type, so the
 * shape consumed here is declared locally; the wire is camelCase fields and
 * `kind`-tagged fact enums.
 */
export interface ServedCliProfile {
  id: string;
  displayName: string;
  programs: string[];
  resume?: { kind: "by_id_argv"; template: string[] } | { kind: "none" } | { kind: "unknown" };
  handshake?: {
    success?: string[];
    failure?: string[];
    successRegex?: string[];
    failureRegex?: string[];
    titleRegex?: string[];
  };
  usageLimitPhrases?: string[];
  restoreTier?: "full" | "terminal_only";
}

/** The placeholder a `by_id_argv` resume template carries for the session id. */
const ID_PLACEHOLDER = "{id}";

/** Build the descriptor for one served profile. Regexes compile once, here. */
export function descriptorFromProfile(profile: ServedCliProfile): SessionProviderDescriptor {
  const hp = profile.handshake ?? {};
  const patterns: HandshakePatterns = {
    success: hp.success ?? [],
    failure: hp.failure ?? [],
    successPatterns: (hp.successRegex ?? []).map((src) => new RegExp(src, "i")),
    failurePatterns: (hp.failureRegex ?? []).map((src) => new RegExp(src, "i")),
    titlePatterns: (hp.titleRegex ?? []).map((src) => new RegExp(src, "i")),
  };
  const template = profile.resume?.kind === "by_id_argv" ? profile.resume.template : null;
  // Mirrors `cli_profile::restore_tier`: a Full claim is only honoured when
  // the profile also says HOW to resume.
  const tier: RestoreTier =
    profile.restoreTier === "full" && template !== null ? "full" : "terminal-only";
  return {
    provider: profile.id,
    resumeCommand: (sessionId: string) =>
      template === null ? null : template.map((arg) => arg.split(ID_PLACEHOLDER).join(sessionId)),
    handshakePatterns: () => patterns,
    restoreTier: () => tier,
  };
}

/** Primed cache: provider id → descriptor. `null` until profiles arrive. */
let descriptors: Map<string, SessionProviderDescriptor> | null = null;
/** The in-flight load, so concurrent callers share one IPC round trip. */
let inflight: Promise<boolean> | null = null;

/**
 * Prime the cache from served profiles. Exported for callers that already
 * hold the list (and for tests, which prime from the checked-in snapshot the
 * Rust suite pins to the live manifest).
 */
export function setCliProfiles(profiles: readonly ServedCliProfile[]): void {
  descriptors = new Map(profiles.map((p) => [p.id, descriptorFromProfile(p)]));
}

/** Drop the cache (tests). */
export function resetCliProfiles(): void {
  descriptors = null;
  inflight = null;
}

/** True once served profiles are cached. */
export function cliProfilesLoaded(): boolean {
  return descriptors !== null;
}

/**
 * Fetch the served profiles once and cache them. Resolves `true` when the
 * cache is primed. A failed fetch resolves `false`, logs why, and leaves the
 * cache unprimed so the next call retries — while unprimed, every provider
 * reads as unknown (terminal-only), never as Claude.
 */
export function loadCliProfiles(): Promise<boolean> {
  if (descriptors !== null) return Promise.resolve(true);
  if (inflight !== null) return inflight;
  inflight = invoke<unknown>("terminal_cli_profiles")
    .then((served) => {
      if (!Array.isArray(served)) {
        console.warn(
          "[providerAdapter] terminal_cli_profiles returned no profile list — every provider " +
            "restores terminal-only until it loads",
          served,
        );
        return false;
      }
      setCliProfiles(served as ServedCliProfile[]);
      return true;
    })
    .catch((err: unknown) => {
      console.warn(
        "[providerAdapter] terminal_cli_profiles failed — every provider restores terminal-only " +
          "until it loads:",
        err,
      );
      return false;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/**
 * The descriptor for `provider`, or `null` when the runner serves no profile
 * for it — including every provider while the cache is unprimed. `null` means
 * UNKNOWN: restore the terminal only, and name the provider; never substitute
 * another CLI's descriptor.
 */
export function providerDescriptorFor(
  provider: string | undefined,
): SessionProviderDescriptor | null {
  if (descriptors === null || provider === undefined) return null;
  return descriptors.get(provider) ?? null;
}
