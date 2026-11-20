// Tests for scripts/lib/repo-local-git-env.mjs — run with
// `node --test scripts/__tests__/repo-local-git-env.test.mjs`
// (or `pnpm run provenance:test`, which runs it beside frontend-provenance's).
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  EXPECTED_COUNT,
  LIST_FILE,
  parseRepoLocalGitEnv,
  readRepoLocalGitEnv,
  withoutRepoLocalGitEnv,
} from "../lib/repo-local-git-env.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const LIB = path.join(here, "../lib/repo-local-git-env.mjs");

test("reads the crate's list: 13 names, including every name the old copies scrubbed", () => {
  assert.equal(
    LIST_FILE,
    path.resolve(here, "../../src-tauri/src/git_posture/repo_local_git_env.rs"),
  );
  const names = readRepoLocalGitEnv();
  assert.equal(names.length, EXPECTED_COUNT);
  assert.equal(EXPECTED_COUNT, 13);
  for (const n of ["GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_CONFIG"]) {
    assert.ok(names.includes(n), `${n} must be on the list: ${names}`);
  }
  // Command-scope config is deliberately NOT on it (git_posture's
  // COMMAND_SCOPE_GIT_CONFIG_ENV): an env-injected safe.directory rides on it.
  assert.ok(!names.includes("GIT_CONFIG_PARAMETERS"));
  assert.ok(!names.includes("GIT_CONFIG_COUNT"));
});

test("a file the reader no longer understands throws instead of scrubbing less", () => {
  const twelve = readRepoLocalGitEnv().filter((n) => n !== "GIT_SHALLOW_FILE");
  const quoted = (names) => names.map((n) => `    "${n}",\n`).join("");
  assert.throws(() => parseRepoLocalGitEnv(quoted(twelve)), /expected 13 .* found 12/);
  assert.throws(
    () => parseRepoLocalGitEnv(quoted([...twelve, "GIT_SHALLOW_FILE", "GIT_EXTRA"])),
    /found 14/,
  );
  assert.throws(
    () =>
      parseRepoLocalGitEnv(quoted([...twelve.filter((n) => n !== "GIT_DIR"), "GIT_A", "GIT_B"])),
    /including GIT_DIR/,
  );
  assert.throws(() => parseRepoLocalGitEnv(quoted([...twelve, "GIT_DIR"])), /listed twice/);
  assert.throws(() => parseRepoLocalGitEnv(""), /found 0 \(none\)/);
  assert.deepEqual(parseRepoLocalGitEnv(quoted([...twelve, "GIT_SHALLOW_FILE"])), [
    ...twelve,
    "GIT_SHALLOW_FILE",
  ]);
});

test("the scrub drops every name and keeps command-scope config", () => {
  const env = { PATH: "/bin", GIT_CONFIG_PARAMETERS: "'baz.q'='p'", GIT_CONFIG_COUNT: "1" };
  for (const n of readRepoLocalGitEnv()) env[n] = "/decoy";
  assert.deepEqual(withoutRepoLocalGitEnv(env), {
    PATH: "/bin",
    GIT_CONFIG_PARAMETERS: "'baz.q'='p'",
    GIT_CONFIG_COUNT: "1",
  });
});

// End to end: a git child given the scrubbed environment answers about the
// repo `-C` names even when the parent inherited a hook's GIT_DIR. The
// unscrubbed control proves the decoy really redirects git, so the first
// assertion cannot pass vacuously.
test("an inherited GIT_DIR no longer redirects git -C", () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "repo-local-env-"));
  const clean = withoutRepoLocalGitEnv(process.env);
  const repo = (name, file) => {
    const dir = path.join(base, name);
    const g = (...a) => execFileSync("git", ["-C", dir, ...a], { stdio: "pipe", env: clean });
    execFileSync("git", ["init", "-q", dir], { stdio: "pipe", env: clean });
    g("config", "user.email", "t@example.com");
    g("config", "user.name", "t");
    writeFileSync(path.join(dir, file), `${name}\n`);
    g("add", "-A");
    g("commit", "-qm", name);
    return { dir, head: g("rev-parse", "HEAD").toString().trim() };
  };
  try {
    const target = repo("target", "a.txt");
    const decoy = repo("decoy", "b.txt");
    const headOf = (scrub) =>
      execFileSync(
        process.execPath,
        [
          "--input-type=module",
          "-e",
          `import { execFileSync } from "node:child_process";
           import { withoutRepoLocalGitEnv } from ${JSON.stringify(pathToFileURL(LIB).href)};
           const env = ${scrub} ? withoutRepoLocalGitEnv(process.env) : process.env;
           process.stdout.write(execFileSync("git", ["-C", process.argv[1], "rev-parse", "HEAD"], { env }));`,
          target.dir,
        ],
        { env: { ...clean, GIT_DIR: path.join(decoy.dir, ".git") }, stdio: "pipe" },
      )
        .toString()
        .trim();
    assert.equal(headOf(false), decoy.head, "control: the inherited GIT_DIR must redirect git");
    assert.equal(headOf(true), target.head);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});
