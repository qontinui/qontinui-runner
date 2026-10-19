/**
 * termhost-bench primary-state neutrality (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, D12).
 *
 * "Primary-state neutrality is checked, not assumed": before and after every
 * runner arm the harness records SHA-256 digests of the PRIMARY runner's
 * `settings.json`, `paired_user.json` and `auth_tokens.enc`. Any change is an
 * incident that STOPS the run and is reported, never smoothed over — the measured
 * hazard (a temp runner built from the wrong source renaming the primary's
 * `paired_user.json`) is silent and lands about four hours later, the worst
 * quadrant of served policy `operating-rules` `what-makes-an-action-destructive`.
 *
 * The incident never attributes the change to the harness by assumption:
 * qontinui-runner#1769 means a concurrent agent's runner test binary can rewrite
 * the primary's `settings.json`, so a change reads "changed during run, writer
 * unattributed".
 *
 * Where the primary keeps those files (it runs with no `QONTINUI_CONFIG_DIR` /
 * `QONTINUI_SECURE_STORAGE_DIR` override):
 *   - `settings.json`: `dirs::config_dir()/com.qontinui.runner/` — runner
 *     `src-tauri/src/settings.rs` `resolve_config_dir_from`;
 *   - `paired_user.json`, `auth_tokens.enc`: `dirs::data_local_dir()/com.qontinui.runner/`
 *     — runner `src-tauri/src/auth.rs` `default_binding_tenant_probe`.
 * A file that is absent before and after is unchanged (a machine with no primary,
 * such as a hosted CI VM, still runs the check — and it would catch a subject
 * runner writing to the machine-global location, which is a real isolation bug).
 */

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";

/** The three primary-state files D12 names. */
export const PRIMARY_STATE_FILES = Object.freeze(["settings.json", "paired_user.json", "auth_tokens.enc"]);

/**
 * `dirs::config_dir()` and `dirs::data_local_dir()` as the `dirs` crate resolves
 * them, for the HARNESS's user (the primary runs as the same user).
 *
 * @param {{platform?: string, env?: object, home?: string}} [o]
 * @returns {{configDir: string, dataLocalDir: string}}
 */
export function platformDirs({ platform = process.platform, env = process.env, home = homedir() } = {}) {
  const abs = (v) => (typeof v === "string" && v !== "" && isAbsolute(v) ? v : null);
  if (platform === "win32") {
    return {
      configDir: abs(env.APPDATA) ?? join(home, "AppData", "Roaming"),
      dataLocalDir: abs(env.LOCALAPPDATA) ?? join(home, "AppData", "Local"),
    };
  }
  if (platform === "darwin") {
    const support = join(home, "Library", "Application Support");
    return { configDir: support, dataLocalDir: support };
  }
  return {
    configDir: abs(env.XDG_CONFIG_HOME) ?? join(home, ".config"),
    dataLocalDir: abs(env.XDG_DATA_HOME) ?? join(home, ".local", "share"),
  };
}

/**
 * The primary's three state files.
 *
 * @param {{platform?: string, env?: object, home?: string}} [o]
 * @returns {{label: string, path: string}[]}
 */
export function primaryStatePaths(o = {}) {
  const { configDir, dataLocalDir } = platformDirs(o);
  const app = "com.qontinui.runner";
  return [
    { label: "settings.json", path: join(configDir, app, "settings.json") },
    { label: "paired_user.json", path: join(dataLocalDir, app, "paired_user.json") },
    { label: "auth_tokens.enc", path: join(dataLocalDir, app, "auth_tokens.enc") },
  ];
}

/**
 * One file's digest: present (with `sha256:<hex>`), absent, or unreadable.
 *
 * @param {string} path
 * @param {{read?: (p: string) => Buffer}} [deps]
 */
export function digestFile(path, { read = readFileSync } = {}) {
  try {
    const bytes = read(path);
    return { state: "present", sha256: `sha256:${createHash("sha256").update(bytes).digest("hex")}` };
  } catch (e) {
    if (e?.code === "ENOENT" || e?.code === "ENOTDIR") return { state: "absent" };
    return { state: "unreadable", error: e?.code ?? e?.message ?? String(e) };
  }
}

/**
 * @param {{label: string, path: string}[]} paths
 * @param {{read?: (p: string) => Buffer}} [deps]
 */
export function snapshot(paths, deps = {}) {
  return paths.map((p) => ({ ...p, ...digestFile(p.path, deps) }));
}

/** A digest's comparable identity. */
function identity(d) {
  return d.state === "present" ? d.sha256 : d.state;
}

/**
 * The files whose digest differs between two snapshots of the same paths.
 *
 * @returns {{label: string, path: string, before: string, after: string}[]}
 */
export function diffSnapshots(before, after) {
  const out = [];
  for (const b of before) {
    const a = after.find((x) => x.path === b.path);
    const was = identity(b);
    const now = a ? identity(a) : "missing-from-snapshot";
    if (was !== now) out.push({ label: b.label, path: b.path, before: was, after: now });
  }
  return out;
}

/**
 * What the guard can protect, from its baseline: OPERATIVE when it watches at
 * least one primary-state file, or when the machine has no primary at all (every
 * file ABSENT and nothing on :9876 — nothing to protect, which the verdict says).
 * UNKNOWN when none is watched but a primary is, or may be, present
 * (`primaryPresent` true or null), or a file exists that cannot be read (state the
 * guard cannot see is not state that is not there): a guard watching nothing would
 * pass every check, so the result is not "unchanged" but unknown.
 *
 * @param {{label: string, state: string, sha256?: string}[]} base the baseline snapshot
 * @param {boolean | null} primaryPresent :9876 listening (true), refused (false), or unknown (null)
 * @returns {{state: "OPERATIVE" | "UNKNOWN", reason: string, watched: number}}
 */
export function neutralityVerdict(base, primaryPresent) {
  const files = base.map((d) => `${d.label} ${d.state === "present" ? `${String(d.sha256).slice(0, 19)}…` : d.state}`).join("; ");
  const watched = base.filter((d) => d.state === "present").length;
  if (watched > 0) return { state: "OPERATIVE", reason: `watching ${watched} of ${base.length} primary-state file(s): ${files}`, watched };
  const unreadable = base.filter((d) => d.state === "unreadable").length;
  if (unreadable > 0) {
    return { state: "UNKNOWN", reason: `${unreadable} primary-state file(s) exist but cannot be read (${files}): the D12 guard would watch nothing`, watched };
  }
  if (primaryPresent === false) {
    return { state: "OPERATIVE", reason: `no primary runner on this machine (nothing listens on :9876; ${files}): nothing to protect`, watched };
  }
  return {
    state: "UNKNOWN",
    reason:
      `a primary runner ${primaryPresent === true ? "is" : "may be"} present (:9876 ${primaryPresent === true ? "listens" : "did not answer either way"}) ` +
      `but none of its state files could be read (${files}): the D12 guard would watch nothing`,
    watched,
  };
}

/**
 * Baseline once, then `check(stage)` before and after every runner arm.
 */
export class NeutralityGuard {
  /**
   * @param {{paths?: {label: string, path: string}[], read?: (p: string) => Buffer, now?: () => Date}} [o]
   */
  constructor({ paths = primaryStatePaths(), read, now = () => new Date() } = {}) {
    this.paths = paths;
    this.deps = read ? { read } : {};
    this.now = now;
    this.base = null;
    this.checks = 0;
  }

  /** Take (or retake) the baseline. */
  baseline() {
    this.base = snapshot(this.paths, this.deps);
    return this.base;
  }

  /**
   * Compare against the baseline.
   *
   * @param {string} stage e.g. `before unit 3 arm focus`
   * @returns {{changed: false, unreadable: string[]} |
   *   {changed: true, changes: object[], incident: object}}
   */
  check(stage) {
    if (!this.base) this.baseline();
    this.checks += 1;
    const current = snapshot(this.paths, this.deps);
    const changes = diffSnapshots(this.base, current);
    const unreadable = current.filter((d) => d.state === "unreadable").map((d) => d.label);
    if (changes.length === 0) return { changed: false, unreadable };
    const what = changes.map((c) => `${c.label} ${short(c.before)} -> ${short(c.after)}`).join("; ");
    return {
      changed: true,
      changes,
      incident: {
        kind: "primary_state_changed",
        at: this.now().toISOString(),
        detail:
          `primary runner state changed during the run (${stage}): ${what}. Writer unattributed ` +
          `(qontinui-runner#1769 lets a concurrent runner test binary rewrite settings.json); the run stopped.`,
        stops_run: true,
        data: { stage, changes },
      },
    };
  }
}

function short(identityValue) {
  return identityValue.startsWith("sha256:") ? `${identityValue.slice(0, 19)}…` : identityValue;
}
