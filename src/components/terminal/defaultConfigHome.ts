/**
 * The DEFAULT Claude account home (`<home>/.claude`), and the one rule for
 * typing an account into a resume: the default home is NEVER typed as
 * `CLAUDE_CONFIG_DIR`.
 *
 * Why: Claude Code keeps the default account's global config at
 * `~/.claude.json`, OUTSIDE `~/.claude`. Setting `CLAUDE_CONFIG_DIR=~/.claude`
 * makes it read `~/.claude/.claude.json` instead — a different (usually empty)
 * config — so the resume runs as a stranger to the session (Rust
 * `discovery::is_default_config_home` documents the same).
 *
 * The path comes from the backend (`claude_default_config_home`), never from a
 * `$HOME` guess here. Every typed resume — the boot restore, every one-click
 * Resume, the "Since restart" bulk resume, a zone-profile resume — normalises
 * through {@link typedConfigDir} in `runVerifiedResume`, the one place a resume
 * command is built and typed. Only the REGISTRY record keeps the explicit path
 * (so a session resumed under the default home stops reading as "account
 * unknown"); a tab, the per-tab last-known id and a zone profile carry the
 * TYPED form ({@link resolveAccountDir}'s `typedDir`), so the explicit path can
 * never leak back into a command from them.
 *
 * When the home cannot be read, a dir that MAY be it ({@link mayBeDefaultConfigHome})
 * is never typed and never offered as an account to type: honesty over guessing.
 */

import { invoke } from "@tauri-apps/api/core";

let cached: Promise<string | null> | null = null;

/**
 * The default home, read once per page load. `null` when the backend cannot
 * resolve a home dir or the read failed (a failed read is retried next call).
 */
export function loadDefaultConfigHome(): Promise<string | null> {
  if (cached === null) {
    const read = invoke<string | null>("claude_default_config_home")
      .then((home) => (typeof home === "string" && home.trim() ? home : null))
      .catch((err: unknown) => {
        console.warn("[resume] could not read the default Claude home:", err);
        cached = null;
        return null;
      });
    cached = read;
  }
  return cached;
}

function isWindowsPlatform(): boolean {
  return typeof navigator !== "undefined" && (navigator.platform ?? "").startsWith("Win");
}

/** Path comparison form: forward slashes, no trailing slash, case-folded on Windows. */
function comparable(path: string, windows: boolean): string {
  const p = path.trim().replace(/\\/g, "/").replace(/\/+$/, "");
  return windows ? p.toLowerCase() : p;
}

/** Is `configDir` the default home `home`? */
export function isDefaultConfigHome(
  configDir: string,
  home: string | null,
  windows: boolean = isWindowsPlatform(),
): boolean {
  return home !== null && comparable(configDir, windows) === comparable(home, windows);
}

/**
 * The `CLAUDE_CONFIG_DIR` to TYPE for `configDir`: `undefined` for none and for
 * the default home, else the dir unchanged.
 */
export function typedConfigDir(
  configDir: string | undefined,
  home: string | null,
  windows: boolean = isWindowsPlatform(),
): string | undefined {
  if (!configDir?.trim()) return undefined;
  return isDefaultConfigHome(configDir, home, windows) ? undefined : configDir;
}

/**
 * Could `dir` be the default home, judged without knowing the home? Its last
 * path segment is `.claude` — the only shape `<home>/.claude` can take. Used
 * only when {@link loadDefaultConfigHome} answered `null`.
 */
export function mayBeDefaultConfigHome(
  dir: string,
  windows: boolean = isWindowsPlatform(),
): boolean {
  const last = comparable(dir, windows).split("/").pop() ?? "";
  return last === ".claude";
}

/**
 * Validate a config dir path before it is TYPED into a shell — reject shell
 * metacharacters (and anything non-ASCII). `undefined` = rejected (or absent).
 * Applied only to a dir that will actually be typed: the default home never is.
 */
const SAFE_PATH_RE = /^[a-zA-Z0-9_\-./\\: ]+$/;
export function sanitizeConfigDir(dir: string | undefined): string | undefined {
  if (!dir) return undefined;
  return SAFE_PATH_RE.test(dir) ? dir : undefined;
}

/** Why a recorded account dir cannot be resumed under. */
export type AccountDirRejection =
  /** Not the default home, and not shell-safe to type. */
  | "config-dir-rejected"
  /** The default home is unreadable and this dir may be it — never typed on a guess. */
  | "default-home-unknown";

/**
 * A known account dir, split into what each consumer may carry:
 * `recordDir` — the explicit path, for the REGISTRY record only;
 * `typedDir` — what a resume types as `CLAUDE_CONFIG_DIR`, and what a tab, the
 * last-known id and a zone profile carry: `undefined` for the default home.
 */
export type AccountDirResolution =
  | { kind: "resolved"; recordDir: string; typedDir: string | undefined }
  | { kind: "unknown"; reason: AccountDirRejection };

/**
 * THE rule for resuming under a recorded (non-blank) account dir:
 * - the default home → resolved, typed as nothing. NOT shell-checked: it is
 *   never typed, so a home with an apostrophe or a non-ASCII name still resumes;
 * - home unknown and the dir may be the default home → unknown;
 * - otherwise typed as itself when shell-safe, else unknown.
 */
export function resolveAccountDir(
  dir: string,
  home: string | null,
  windows: boolean = isWindowsPlatform(),
): AccountDirResolution {
  const recordDir = dir.trim();
  if (isDefaultConfigHome(recordDir, home, windows)) {
    return { kind: "resolved", recordDir, typedDir: undefined };
  }
  if (home === null && mayBeDefaultConfigHome(recordDir, windows)) {
    return { kind: "unknown", reason: "default-home-unknown" };
  }
  const safe = sanitizeConfigDir(recordDir);
  return safe
    ? { kind: "resolved", recordDir: safe, typedDir: safe }
    : { kind: "unknown", reason: "config-dir-rejected" };
}

/**
 * The account dirs an operator may pick to TYPE (the chooser), the default
 * home excluded — it has its own "default" entry, typed as no variable. With
 * the home unknown, every dir that MAY be it is excluded too, rather than
 * listed as a dir to type.
 */
export function accountRosterOf(
  configDirs: readonly string[],
  home: string | null,
  windows: boolean = isWindowsPlatform(),
): string[] {
  return configDirs.filter((d) =>
    home === null ? !mayBeDefaultConfigHome(d, windows) : !isDefaultConfigHome(d, home, windows),
  );
}
