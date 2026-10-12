/**
 * The pinned, portable VS Code the `vscode` setup measures (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, D5 and Phase 4):
 * one stable version, its archives' official SHA-256 from Microsoft's update API,
 * a cache OUTSIDE any repository, verification on every use, and each archive
 * unpacked into its own new directory inside the run directory.
 *
 * Provenance of the pin (read 2026-10-12): the update API's
 * `https://update.code.visualstudio.com/api/versions/1.139.1/<platform>/stable`
 * answers each archive's `url` and `sha256hash`; 1.139.1 is the patch release of
 * 1.139, published 2026-09-25 (`timestamp` 1790308685731). Both archives were
 * downloaded and hashed to those values, and the Windows one unpacked (never
 * run) to read its layout: `Code.exe` at the root, the app under
 * `<commit[0:10]>/resources/app`, `product.json` version 1.139.1 at commit
 * 04c0d99f4f…, Electron 43.6.0, and the bundled ConPTY
 * (`node_modules.asar.unpacked/node-pty/build/Release/conpty/{conpty.dll,OpenConsole.exe}`).
 * To move the pin, re-read that API for the new version and replace the table;
 * the tests check its shape, never the network.
 *
 * Every download is hashed while it streams and REFUSED on a mismatch — the
 * partial file is deleted and nothing is unpacked; a cached archive is re-hashed
 * before each use and replaced when it no longer matches.
 */

import { spawnSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { createReadStream, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { open } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join, parse as parsePath, posix, resolve, win32 } from "node:path";

import { assertInside } from "../rundir.mjs";

/** The pinned build. */
export const VSCODE_PIN = Object.freeze({
  version: "1.139.1",
  commit: "04c0d99f4fb0d8afe6ce4f0c58e31e183ac3e4b1",
  released: "2026-09-25",
  builds: Object.freeze({
    "win32-x64": Object.freeze({
      key: "win32-x64",
      updatePlatform: "win32-x64-archive",
      url: "https://vscode.download.prss.microsoft.com/dbazure/download/stable/04c0d99f4fb0d8afe6ce4f0c58e31e183ac3e4b1/VSCode-win32-x64-1.139.1.zip",
      fallbackUrl: "https://update.code.visualstudio.com/1.139.1/win32-x64-archive/stable",
      sha256: "2186d345470644f232f6682d4a9a18f335e96e0233eb29c9ab005dc4dee0220e",
      bytes: 336761238,
      archive: "VSCode-win32-x64-1.139.1.zip",
      kind: "zip",
      exe: "Code.exe",
      product: "04c0d99f4f/resources/app/product.json",
      chromeSandbox: null,
    }),
    "linux-x64": Object.freeze({
      key: "linux-x64",
      updatePlatform: "linux-x64",
      url: "https://vscode.download.prss.microsoft.com/dbazure/download/stable/04c0d99f4fb0d8afe6ce4f0c58e31e183ac3e4b1/code-stable-x64-1790309351.tar.gz",
      fallbackUrl: "https://update.code.visualstudio.com/1.139.1/linux-x64/stable",
      sha256: "0c9cbcea070e9a0fc62de5c78fee01a80f8e32f78ada664d9731f6c517503b7e",
      bytes: 341831423,
      archive: "code-stable-x64-1790309351.tar.gz",
      kind: "tar.gz",
      exe: "VSCode-linux-x64/code",
      product: "VSCode-linux-x64/resources/app/product.json",
      chromeSandbox: "VSCode-linux-x64/chrome-sandbox",
    }),
  }),
});

/** The update-API record a build's hash was read from. */
export function metadataUrl(build, version = VSCODE_PIN.version) {
  return `https://update.code.visualstudio.com/api/versions/${version}/${build.updatePlatform}/stable`;
}

/** The pinned build for this platform, or null. */
export function buildFor(platform = process.platform, arch = process.arch) {
  return VSCODE_PIN.builds[`${platform}-${arch}`] ?? null;
}

/** The marker a finished unpack leaves, naming the archive it came from. */
export const UNPACK_MARKER = ".termhost-unpacked.json";

/** An archive whose bytes are not the ones Microsoft published. */
export class ArchiveIntegrityError extends Error {
  constructor({ url, expected, actual, bytes }) {
    super(`SHA-256 mismatch for ${url}: expected ${expected}, got ${actual} over ${bytes} bytes — refused, nothing unpacked`);
    this.name = "ArchiveIntegrityError";
    this.url = url;
    this.expected = expected;
    this.actual = actual;
    this.bytes = bytes;
  }
}

/**
 * Where archives are cached by default — per user, outside any checkout:
 * `%LOCALAPPDATA%\termhost-bench\cache\vscode` on Windows,
 * `${XDG_CACHE_HOME:-~/.cache}/termhost-bench/vscode` elsewhere.
 */
export function defaultCacheDir({ platform = process.platform, env = process.env, home = homedir() } = {}) {
  if (platform === "win32") {
    return win32.join(env.LOCALAPPDATA || win32.join(home, "AppData", "Local"), "termhost-bench", "cache", "vscode");
  }
  return posix.join(env.XDG_CACHE_HOME || posix.join(home, ".cache"), "termhost-bench", "vscode");
}

/**
 * The git checkout `dir` lies in (the nearest ancestor holding `.git`), or null.
 * A cache inside a checkout would put a third-party binary in a repository.
 */
export function findEnclosingRepo(dir, { exists = existsSync } = {}) {
  let cur = resolve(dir);
  const { root } = parsePath(cur);
  for (;;) {
    if (exists(join(cur, ".git"))) return cur;
    if (cur === root) return null;
    const up = dirname(cur);
    if (up === cur) return null;
    cur = up;
  }
}

/** SHA-256 of a file, streamed. */
export function sha256File(path) {
  return new Promise((resolveHash, reject) => {
    const hash = createHash("sha256");
    createReadStream(path)
      .on("data", (c) => hash.update(c))
      .on("error", reject)
      .on("end", () => resolveHash(hash.digest("hex")));
  });
}

/**
 * Download one archive to `dest`, hashing as it streams; on a mismatch delete the
 * partial file and throw {@link ArchiveIntegrityError} (never retried from the
 * next URL — the bytes were wrong, not unreachable). A network failure moves on
 * to the next URL.
 *
 * @param {{urls: string[], dest: string, expected: string, fetch?: typeof fetch,
 *   log?: (s: string) => void, timeoutMs?: number}} o
 * @returns {Promise<{path: string, bytes: number, url: string, sha256: string}>}
 */
export async function downloadVerified({ urls, dest, expected, fetch = globalThis.fetch, log = () => {}, timeoutMs = 15 * 60000 }) {
  if (!/^[0-9a-f]{64}$/.test(expected ?? "")) throw new TypeError(`expected SHA-256 must be 64 lowercase hex, got ${expected}`);
  mkdirSync(dirname(dest), { recursive: true });
  const errors = [];
  for (const url of urls.filter(Boolean)) {
    const part = `${dest}.part-${process.pid}-${randomBytes(3).toString("hex")}`;
    let fh = null;
    try {
      log(`vscode: downloading ${url}`);
      const res = await fetch(url, { redirect: "follow", signal: AbortSignal.timeout(timeoutMs) });
      if (!res.ok || !res.body) {
        errors.push(`${url}: HTTP ${res.status}`);
        continue;
      }
      const hash = createHash("sha256");
      fh = await open(part, "wx");
      let bytes = 0;
      for await (const chunk of res.body) {
        const buf = Buffer.from(chunk);
        hash.update(buf);
        await fh.write(buf);
        bytes += buf.length;
      }
      await fh.close();
      fh = null;
      const actual = hash.digest("hex");
      if (actual !== expected) throw new ArchiveIntegrityError({ url, expected, actual, bytes });
      renameSync(part, dest);
      log(`vscode: ${bytes} bytes, SHA-256 ${actual} verified`);
      return { path: dest, bytes, url, sha256: actual };
    } catch (e) {
      if (fh) await fh.close().catch(() => {});
      rmSync(part, { force: true });
      if (e instanceof ArchiveIntegrityError) throw e;
      errors.push(`${url}: ${e?.message ?? e}`);
    }
  }
  throw new Error(`could not download the pinned VS Code archive (${errors.join("; ") || "no URL"})`);
}

/**
 * The pinned archive in `cacheDir`, downloaded when absent, and verified either
 * way: a cached file whose hash no longer matches is discarded and fetched again.
 *
 * @param {{build: object, cacheDir: string, version?: string, fetch?: typeof fetch,
 *   log?: (s: string) => void, hash?: (p: string) => Promise<string>, exists?: (p: string) => boolean}} o
 * @returns {Promise<{path: string, cached: boolean, sha256: string}>}
 */
export async function ensureArchive({ build, cacheDir, version = VSCODE_PIN.version, fetch, log = () => {}, hash = sha256File, exists = existsSync }) {
  const repo = findEnclosingRepo(cacheDir, { exists });
  if (repo) throw new Error(`the archive cache ${cacheDir} is inside the git checkout ${repo}; caches live outside any repository (D5)`);
  const dest = join(cacheDir, version, build.archive);
  if (exists(dest)) {
    const actual = await hash(dest);
    if (actual === build.sha256) return { path: dest, cached: true, sha256: actual };
    log(`vscode: cached ${dest} hashes to ${actual}, not the pinned ${build.sha256}: discarding it`);
    rmSync(dest, { force: true });
  }
  const r = await downloadVerified({ urls: [build.url, build.fallbackUrl], dest, expected: build.sha256, fetch, log });
  return { path: r.path, cached: false, sha256: r.sha256 };
}

/**
 * The extraction command for one archive kind: Windows' own bsdtar
 * (`%SystemRoot%\System32\tar.exe`, which reads ZIP and refuses absolute and
 * `..` member paths), else Windows PowerShell's `Expand-Archive` with the paths
 * passed in the environment, never spliced into the script; GNU tar for the
 * Linux tarball.
 *
 * @returns {{cmd: string, args: string[], env?: object}}
 */
export function unpackCommand({ kind, archive, dest, platform = process.platform, env = process.env, exists = existsSync }) {
  if (kind === "zip") {
    if (platform !== "win32") throw new Error("the ZIP build unpacks on Windows");
    const tar = win32.join(env.SystemRoot || env.windir || "C:\\Windows", "System32", "tar.exe");
    if (exists(tar)) return { cmd: tar, args: ["-xf", archive, "-C", dest] };
    return {
      cmd: "powershell.exe",
      args: [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath $env:TERMHOST_UNPACK_ARCHIVE -DestinationPath $env:TERMHOST_UNPACK_DEST -Force",
      ],
      env: { ...env, TERMHOST_UNPACK_ARCHIVE: archive, TERMHOST_UNPACK_DEST: dest },
    };
  }
  if (kind === "tar.gz") return { cmd: "tar", args: ["-xzf", archive, "-C", dest] };
  throw new RangeError(`unknown archive kind ${kind}`);
}

/** `<dir>/resources/app/product.json` up to two directory levels down, or null. */
export function findProductJson(dir, { exists = existsSync, list = (d) => readdirSync(d, { withFileTypes: true }) } = {}) {
  const at = (d) => join(d, "resources", "app", "product.json");
  const queue = [{ d: dir, depth: 0 }];
  while (queue.length > 0) {
    const { d, depth } = queue.shift();
    if (exists(at(d))) return at(d);
    if (depth >= 2) continue;
    let entries = [];
    try {
      entries = list(d);
    } catch {
      continue;
    }
    for (const e of entries) if (e.isDirectory() && !e.name.startsWith(".")) queue.push({ d: join(d, e.name), depth: depth + 1 });
  }
  return null;
}

/** `{version, commit, nameShort, ...}` from a product.json. */
export function readProduct(path, { read = readFileSync } = {}) {
  const p = JSON.parse(read(path, "utf8"));
  if (typeof p.version !== "string" || typeof p.commit !== "string") throw new Error(`${path} names no version and commit`);
  return p;
}

/**
 * The executable and product.json of an unpacked pinned build.
 *
 * @returns {{exe: string, productJson: string, chromeSandbox: string | null}}
 */
export function locateApp(appDir, build, { exists = existsSync } = {}) {
  const exe = join(appDir, ...build.exe.split("/"));
  if (!exists(exe)) throw new Error(`no ${build.exe} in ${appDir}`);
  const pinned = join(appDir, ...build.product.split("/"));
  const productJson = exists(pinned) ? pinned : findProductJson(appDir, { exists });
  if (!productJson) throw new Error(`no resources/app/product.json under ${appDir}`);
  return { exe, productJson, chromeSandbox: build.chromeSandbox ? join(appDir, ...build.chromeSandbox.split("/")) : null };
}

/**
 * Unpack the verified archive into a NEW directory of this run (once per run;
 * later units of the run reuse it after checking its marker names the same
 * archive), and prove the result is the pinned build.
 *
 * @returns {Promise<string>} the app directory
 */
export async function ensureUnpacked({
  archive,
  build,
  runDir,
  version = VSCODE_PIN.version,
  commit = VSCODE_PIN.commit,
  run = spawnSync,
  platform = process.platform,
  env = process.env,
  log = () => {},
  exists = existsSync,
}) {
  const parent = assertInside(runDir, join(runDir, "vscode-app"));
  const final = join(parent, `${version}-${build.key}`);
  const marker = join(final, UNPACK_MARKER);
  if (exists(marker)) {
    const m = JSON.parse(readFileSync(marker, "utf8"));
    if (m.sha256 !== build.sha256) throw new Error(`${final} was unpacked from ${m.sha256}, not the pinned ${build.sha256}`);
    return final;
  }
  mkdirSync(parent, { recursive: true });
  const staging = mkdtempSync(join(parent, ".unpack-"));
  try {
    const c = unpackCommand({ kind: build.kind, archive, dest: staging, platform, env, exists });
    log(`vscode: unpacking into ${staging}`);
    const res = run(c.cmd, c.args, { encoding: "utf8", timeout: 600000, windowsHide: true, env: c.env ?? env, maxBuffer: 64 * 1024 * 1024 });
    if (res.error || res.status !== 0) {
      throw new Error(`${c.cmd} ${c.args[0]} failed (${res.error?.message ?? `exit ${res.status}`}): ${String(res.stderr ?? "").trim().slice(-400)}`);
    }
    const app = locateApp(staging, build, { exists });
    const product = readProduct(app.productJson);
    if (product.version !== version || product.commit !== commit) {
      throw new Error(`the archive unpacked to VS Code ${product.version} (${product.commit}), not the pinned ${version} (${commit})`);
    }
    writeFileSync(join(staging, UNPACK_MARKER), `${JSON.stringify({ sha256: build.sha256, version, commit, archive })}\n`);
    renameSync(staging, final);
    return final;
  } catch (e) {
    rmSync(staging, { recursive: true, force: true });
    throw e;
  }
}

/**
 * An installed VS Code named by `--vscode-app-path` (its directory or its
 * executable): measured "as configured" (D5), still with a private profile.
 *
 * @returns {{exe: string, appDir: string, productJson: string | null}}
 */
export function resolveAppPath(appPath, { platform = process.platform, exists = existsSync, stat = statSync } = {}) {
  const abs = resolve(appPath);
  let isFile = false;
  try {
    isFile = stat(abs).isFile();
  } catch {
    throw new Error(`--vscode-app-path ${abs} does not exist`);
  }
  if (isFile) return { exe: abs, appDir: dirname(abs), productJson: findProductJson(dirname(abs), { exists }) };
  const names = platform === "win32" ? ["Code.exe"] : ["code", join("VSCode-linux-x64", "code")];
  const exe = names.map((n) => join(abs, n)).find((p) => exists(p));
  if (!exe) throw new Error(`no ${names.join(" or ")} in --vscode-app-path ${abs}`);
  return { exe, appDir: dirname(exe), productJson: findProductJson(dirname(exe), { exists }) };
}

/**
 * Whether Chromium's sandbox can start on this Linux box, and so whether VS Code
 * needs `--no-sandbox`: root always does; otherwise the namespace sandbox needs
 * unprivileged user namespaces (Debian's `unprivileged_userns_clone`, Ubuntu's
 * AppArmor restriction, `max_user_namespaces`), and the fallback SUID sandbox a
 * root-owned setuid `chrome-sandbox` — which an unprivileged tar extraction never
 * produces. An unreadable verdict launches with `--no-sandbox`: a window that
 * never opens measures nothing.
 *
 * @returns {{state: string, reason: string, noSandbox: boolean}}
 */
export function linuxSandboxVerdict({ read = (p) => readFileSync(p, "utf8"), stat = statSync, getuid = process.getuid, chromeSandbox = null } = {}) {
  if (typeof getuid === "function" && getuid() === 0) {
    return { state: "DEGRADED", noSandbox: true, reason: "running as root: Chromium refuses its sandbox there, so VS Code launches with --no-sandbox" };
  }
  const value = (p) => {
    try {
      return String(read(p)).trim();
    } catch {
      return null;
    }
  };
  const clone = value("/proc/sys/kernel/unprivileged_userns_clone");
  const apparmor = value("/proc/sys/kernel/apparmor_restrict_unprivileged_userns");
  const max = value("/proc/sys/user/max_user_namespaces");
  let userns = null;
  let why = "no user-namespace setting could be read";
  if (clone === "0") [userns, why] = [false, "kernel.unprivileged_userns_clone = 0"];
  else if (apparmor === "1") [userns, why] = [false, "kernel.apparmor_restrict_unprivileged_userns = 1"];
  else if (max === "0") [userns, why] = [false, "user.max_user_namespaces = 0"];
  else if (max !== null && Number(max) > 0) [userns, why] = [true, `user.max_user_namespaces = ${max}`];
  if (userns === true) return { state: "OPERATIVE", noSandbox: false, reason: `Chromium's namespace sandbox (${why})` };
  if (chromeSandbox) {
    try {
      const s = stat(chromeSandbox);
      if (s.uid === 0 && (s.mode & 0o4000) !== 0) return { state: "OPERATIVE", noSandbox: false, reason: `the setuid chrome-sandbox (${why})` };
    } catch {
      // absent: no SUID sandbox
    }
  }
  return {
    state: "DEGRADED",
    noSandbox: true,
    reason: `no namespace sandbox (${why}) and ${chromeSandbox ? "chrome-sandbox is not root-owned setuid" : "no setuid chrome-sandbox known yet"}: VS Code launches with --no-sandbox`,
  };
}
