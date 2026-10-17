/**
 * termhost-bench run directories and the D13 session command line.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3: "Each
 * run gets its own directory under the OS temp dir; isolation and teardown follow
 * D5 and D12." Every file a run writes — generator slots and logs, probe records,
 * host configs (tmux.conf), a subject runner's instance root — lives under its run
 * directory, which is created EXCLUSIVELY (an existing directory is an error, never
 * reused), so two runs on one machine can never share a file. {@link assertInside}
 * is the check adapters use before writing.
 *
 * D13: every setup's session is the platform's default interactive shell running
 * the generator with ONE identical command line. {@link generatorArgv} builds it as
 * an argv array (how the harness itself starts processes — never a shell string);
 * {@link commandLine} renders that argv for the one place a string is unavoidable:
 * the text a host types into, or hands to, the session's shell.
 */

import { randomBytes } from "node:crypto";
import { mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

/** The generator every session runs (Phase 1). */
export const GENERATOR_SCRIPT = join(HERE, "generator", "ai-session.mjs");

/** Run ids and generator run ids: what `--termhost-bench-generator` accepts. */
export const RUN_ID_PATTERN = /^[A-Za-z0-9._-]{1,64}$/;

/** Longest run id that still leaves room for a `-u<unit>` generator suffix. */
export const MAX_RUN_ID_LENGTH = 48;

/**
 * A fresh run id: UTC timestamp plus random hex, e.g. `20261012T010203Z-a1b2c3`.
 *
 * @param {Date} [now]
 * @param {string} [rand] hex
 */
export function makeRunId(now = new Date(), rand = randomBytes(3).toString("hex")) {
  const stamp = now
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d+Z$/, "Z");
  const id = `${stamp}-${rand}`;
  if (!RUN_ID_PATTERN.test(id)) throw new Error(`generated run id ${id} is malformed`);
  return id;
}

/** `<base>/termhost-bench` — the parent of every run directory. */
export function benchRoot(base = tmpdir()) {
  return join(resolve(base), "termhost-bench");
}

/**
 * Create the run directory `<base>/termhost-bench/<runId>` exclusively.
 *
 * @param {{runId: string, base?: string}} opts
 * @returns {string} absolute path
 */
export function createRunDir({ runId, base = tmpdir() }) {
  if (!RUN_ID_PATTERN.test(runId) || runId.length > MAX_RUN_ID_LENGTH) {
    throw new RangeError(`bad run id "${runId}" (${RUN_ID_PATTERN}, at most ${MAX_RUN_ID_LENGTH})`);
  }
  const root = benchRoot(base);
  mkdirSync(root, { recursive: true });
  const dir = join(root, runId);
  // No `recursive`: an existing directory throws EEXIST rather than being reused.
  mkdirSync(dir);
  return dir;
}

/**
 * The directory of one unit — one host launch for one (run index, N, setup,
 * variant, build role) — created exclusively inside the run directory, with the
 * generators' run directory `gen/` inside it.
 *
 * @param {string} runDir
 * @param {{index: number, setup: string, variant: string, role: string, n: number, runIndex: number}} unit
 * @returns {{unitDir: string, genDir: string, name: string}}
 */
export function createUnitDir(runDir, unit) {
  const name =
    `u${String(unit.index).padStart(3, "0")}-${unit.setup}-${unit.variant}-${unit.role}` +
    `-n${unit.n}-r${unit.runIndex}`;
  if (!/^[A-Za-z0-9._-]+$/.test(name)) throw new RangeError(`unit name ${name} is not path-safe`);
  const unitDir = join(runDir, "units", name);
  mkdirSync(join(runDir, "units"), { recursive: true });
  mkdirSync(unitDir);
  const genDir = join(unitDir, "gen");
  mkdirSync(genDir);
  return { unitDir, genDir, name };
}

/**
 * Refuse a path outside `dir` (after resolution): the guard every adapter applies
 * before writing a file, so nothing a run writes lands outside its run directory.
 *
 * @param {string} dir
 * @param {string} path
 * @returns {string} the resolved path
 */
export function assertInside(dir, path) {
  const base = resolve(dir);
  const target = resolve(path);
  const rel = relative(base, target);
  if (rel === "" || rel.startsWith("..") || isAbsolute(rel)) {
    throw new Error(`refusing to write ${target}: it is not inside the run directory ${base}`);
  }
  return target;
}

/**
 * The generator run id for one unit: `<runId>-u<index>`. Generators carry it as
 * `--termhost-bench-generator`, which is how the probe excludes them (and their
 * session shells) from the host's process tree, and how teardown finds survivors.
 *
 * @param {string} runId
 * @param {number} unitIndex
 */
export function generatorRunId(runId, unitIndex) {
  const id = `${runId}-u${unitIndex}`;
  if (!RUN_ID_PATTERN.test(id)) throw new RangeError(`generator run id ${id} is malformed`);
  return id;
}

/**
 * The D13 argv — Phase 1 README order:
 * `node <generator> --run-dir <dir> --profile <p> --seed <s> --echo --termhost-bench-generator <id>`.
 * Every session runs with `--echo`; only the focused pane ever receives input.
 *
 * @param {{node?: string, script?: string, genDir: string, profile: string, seed: number, genRunId: string}} o
 * @returns {string[]}
 */
export function generatorArgv({
  node = process.execPath,
  script = GENERATOR_SCRIPT,
  genDir,
  profile,
  seed,
  genRunId,
}) {
  if (!Number.isInteger(seed) || seed < 0 || seed > 0xffffffff) {
    throw new RangeError(`seed must be an integer in 0..4294967295, got ${seed}`);
  }
  if (!RUN_ID_PATTERN.test(genRunId)) throw new RangeError(`bad generator run id ${genRunId}`);
  return [
    node,
    script,
    "--run-dir",
    genDir,
    "--profile",
    profile,
    "--seed",
    String(seed),
    "--echo",
    "--termhost-bench-generator",
    genRunId,
  ];
}

/** POSIX shell quoting: bare when safe, else single quotes with `'\''` escapes. */
export function posixQuote(arg) {
  const s = String(arg);
  if (s !== "" && /^[A-Za-z0-9_\-.,/:=@%+]+$/.test(s)) return s;
  return `'${s.replace(/'/g, "'\\''")}'`;
}

/** PowerShell quoting: a single-quoted literal string ('' escapes '). */
export function powershellQuote(arg) {
  return `'${String(arg).replace(/'/g, "''")}'`;
}

/**
 * Render an argv as one command line for the session's shell.
 *
 * - `posix`: bash/zsh/sh (Linux, macOS, WSL);
 * - `powershell`: Windows PowerShell — the call operator `&` makes a quoted
 *   executable path runnable.
 *
 * @param {string[]} argv
 * @param {"posix" | "powershell"} flavor
 * @returns {string}
 */
export function commandLine(argv, flavor) {
  if (!Array.isArray(argv) || argv.length === 0) throw new TypeError("argv must be a non-empty array");
  if (flavor === "posix") return argv.map(posixQuote).join(" ");
  if (flavor === "powershell") return `& ${argv.map(powershellQuote).join(" ")}`;
  throw new RangeError(`unknown shell flavor ${flavor}`);
}
