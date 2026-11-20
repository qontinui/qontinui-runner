// The repository-local git environment names a runner build's git must NOT
// inherit, read from the ONE list the Rust side compiles:
// `src-tauri/src/git_posture/repo_local_git_env.rs`, which `git_posture.rs`
// (the crate's `git_posture::REPO_LOCAL_GIT_ENV`) and `build.rs` both pull in
// with `include!`. Reading that file instead of copying it means the JavaScript
// provenance readers cannot drift from the crate (plan
// 2026-11-16-runner-build-script-git-spawns-inherit-a-hooks-repo-local-git-env, D1).
//
// Why: a build started from inside a git hook inherits that hook's `GIT_DIR`,
// `GIT_INDEX_FILE`, ... and `git -C <root>` does not override an inherited
// `GIT_DIR`, so every git the build runs would answer about the hook's
// repository — and the build would stamp that repository's sha.
//
// Consumers:
//   * `vite.config.ts` `computeBuildId` — the sha in `dist/build-id.txt`, which
//     `build.rs` re-emits as `RUNNER_BUILD_ID` and `/health` serves as `buildId`;
//   * `scripts/frontend-provenance.mjs` `GIT_ENV` — every git the provenance
//     fold runs.

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** The Rust file holding the list, resolved from this module, not the cwd. */
export const LIST_FILE = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../src-tauri/src/git_posture/repo_local_git_env.rs",
);

/**
 * How many names the list holds today. A git that grows its
 * `--local-env-vars` list fails the Rust test
 * `repo_local_git_env_covers_gits_own_list`; the name added there must raise
 * this in the same change, so a parse that finds a different count is a
 * reader that no longer understands the file, never a list to trust.
 */
export const EXPECTED_COUNT = 13;

/**
 * The names in `source` (the text of `LIST_FILE`): every double-quoted
 * `GIT_[A-Z_]+` literal, in file order. Throws unless there are exactly
 * `EXPECTED_COUNT` distinct names including `GIT_DIR`.
 */
export function parseRepoLocalGitEnv(source, from = LIST_FILE) {
  const names = [...source.matchAll(/"(GIT_[A-Z_]+)"/g)].map((m) => m[1]);
  const distinct = new Set(names);
  if (distinct.size !== names.length) {
    throw new Error(`${from}: a repo-local git env name is listed twice: ${names.join(", ")}`);
  }
  if (names.length !== EXPECTED_COUNT || !distinct.has("GIT_DIR")) {
    throw new Error(
      `${from}: expected ${EXPECTED_COUNT} quoted GIT_* names including GIT_DIR, found ` +
        `${names.length} (${names.join(", ") || "none"}). If the Rust list changed on ` +
        `purpose, change EXPECTED_COUNT in scripts/lib/repo-local-git-env.mjs with it.`,
    );
  }
  return names;
}

/** The names, read from `file` (default: the crate's list). */
export function readRepoLocalGitEnv(file = LIST_FILE) {
  return parseRepoLocalGitEnv(readFileSync(file, "utf8"), file);
}

/**
 * A copy of `env` without any repo-local git name — the environment to hand
 * every git child. Command-scope config (`GIT_CONFIG_PARAMETERS`,
 * `GIT_CONFIG_COUNT` and its numbered pairs) passes through, as it does in the
 * Rust scrub.
 */
export function withoutRepoLocalGitEnv(env = process.env, names = readRepoLocalGitEnv()) {
  const drop = new Set(names);
  return Object.fromEntries(Object.entries(env).filter(([k]) => !drop.has(k)));
}
