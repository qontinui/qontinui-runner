// Which cargo profile `pnpm run build:exe` builds the runner in (plan
// 2026-10-19-primary-runner-is-a-debug-build-and-nothing-shows-it, Phase 3).
//
// The default is `release-checked` -- release's optimization with debug
// assertions kept (see the workspace Cargo.toml). A debug build is an
// announced opt-in, spelled `dev`: `--profile dev` here. The same plan's
// Phases 4 and 5 spell it `-RunnerProfile dev` in dev-start.ps1 and
// `--primary-build-profile dev` in the supervisor. Pure helpers only;
// build-exe.mjs does the I/O.

export const DEFAULT_PROFILE = "release-checked";
export const PROFILE_ENV = "QONTINUI_RUNNER_BUILD_PROFILE";

// What each known profile compiles with. Used for the announcement line only;
// the binary's own /health `provenance.build` reports what was MEASURED.
const KNOWN = {
  "release-checked": { optLevel: "2", debugAssertions: true },
  dev: { optLevel: "0", debugAssertions: true },
  release: { optLevel: "2", debugAssertions: false },
};

/**
 * Resolve the profile from argv (`--profile <name>` / `--profile=<name>`),
 * else the env var, else the default. Returns `{ profile, source }` or
 * `{ error }` for a malformed request -- never a silent fallback.
 */
export function resolveProfile(argv, env) {
  let fromArgv;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--profile") {
      if (i + 1 >= argv.length) return { error: "--profile needs a value" };
      fromArgv = argv[i + 1];
      i++;
    } else if (a.startsWith("--profile=")) {
      fromArgv = a.slice("--profile=".length);
    } else {
      return { error: `unknown argument ${JSON.stringify(a)} (build:exe takes only --profile <name>)` };
    }
  }
  let profile;
  let source;
  if (fromArgv !== undefined) {
    profile = fromArgv;
    source = "--profile";
  } else if (env[PROFILE_ENV] !== undefined && env[PROFILE_ENV].trim() !== "") {
    profile = env[PROFILE_ENV].trim();
    source = PROFILE_ENV;
  } else {
    profile = DEFAULT_PROFILE;
    source = "default";
  }
  // Cargo profile names are case-sensitive; refuse rather than guess.
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(profile)) {
    return { error: `invalid cargo profile name ${JSON.stringify(profile)} (from ${source})` };
  }
  return { profile, source };
}

/**
 * The directory cargo writes a profile's output to: `dev` and `test` ->
 * `debug`, `bench` -> `release`, any other profile -> its own name.
 */
export function profileDir(profile) {
  if (profile === "dev" || profile === "test") return "debug";
  if (profile === "bench") return "release";
  return profile;
}

/** The one line build:exe prints before cargo runs. */
export function announcement(profile) {
  const k = KNOWN[profile];
  const opt = k ? k.optLevel : "unknown";
  const da = k ? (k.debugAssertions ? "on" : "off") : "unknown";
  return `build:exe: profile=${profile} (opt-level ${opt}, debug-assertions ${da})`;
}

/** The WARN printed for a debug opt-in, or null. */
export function debugOptInWarning(profile, source) {
  if (profile !== "dev") return null;
  return (
    `build:exe: WARN: building a DEBUG runner (profile=dev, opt-level 0, chosen by ${source}). ` +
    "Every terminal byte this runner serves is encoded, redacted and emitted unoptimized; " +
    "omit --profile / QONTINUI_RUNNER_BUILD_PROFILE for the optimized release-checked build."
  );
}

/** The cargo argument list for the runner plus its sidecars, in one invocation. */
export function cargoArgs(profile, packages, bins) {
  return [
    "build",
    "--profile",
    profile,
    ...packages.flatMap((p) => ["-p", p]),
    "--bin",
    "qontinui-runner",
    ...bins.flatMap((b) => ["--bin", b]),
    "--features",
    "qontinui-runner/custom-protocol",
    "--message-format=json-render-diagnostics",
  ];
}

/**
 * From cargo's JSON artifact stream, the executable path of each wanted bin
 * (the last compiler-artifact message wins). Bins with no artifact are absent.
 */
export function executablesFromCargoJson(stdout, wanted) {
  const found = new Map();
  for (const line of stdout.split("\n")) {
    const s = line.trim();
    if (!s.startsWith("{")) continue;
    let msg;
    try {
      msg = JSON.parse(s);
    } catch {
      continue;
    }
    if (msg.reason === "compiler-artifact" && msg.target && wanted.includes(msg.target.name) && msg.executable) {
      found.set(msg.target.name, msg.executable);
    }
  }
  return found;
}
