/**
 * Path helpers for the doc finder's directory scan.
 *
 * The scanner used to force Windows separators unconditionally —
 * `root.replace(/\//g, "\\")` on the way in, `${dir}\\${name}` on the way out.
 * That was self-consistent while the root was the hardcoded
 * `C:\Users\<account>\Documents\qontinui-root` literal: a Windows path in, a
 * Windows path out.
 *
 * PR #906 removed the literal and had `TerminalPage` pass the active
 * terminal's `workingDir` instead — a PLATFORM-NATIVE path. On Linux (a
 * shipped target: `ci.yml` runs `tauri build` on `ubuntu-22.04`) that turns
 * `/home/x/proj` into `\home\x\proj`, so `readDir` fails on the root and the
 * modal reports "No document files found" for a directory full of them. The
 * separator has to come from the root, not from an assumption.
 *
 * Kept as a separate module rather than inlined so the branch is unit-testable
 * without a Tauri filesystem — the same split `aiLaunchCommand.ts` uses.
 */

export type PathSeparator = "\\" | "/";

/**
 * Which separator does this root speak?
 *
 * Windows if it is drive-qualified (`C:\…`, `C:/…`), a UNC share
 * (`\\server\share`), or already contains a backslash. Everything else —
 * notably POSIX absolute paths — is forward-slash. A drive-qualified root
 * written with forward slashes still counts as Windows, because `workingDir`
 * frequently arrives that way (transcript `cwd` values are recorded as
 * `C:/Users/…` about as often as `C:\Users\…`).
 */
export function pathSeparatorFor(root: string): PathSeparator {
  if (/^[A-Za-z]:/.test(root)) return "\\";
  if (root.startsWith("\\\\")) return "\\";
  if (root.includes("\\")) return "\\";
  return "/";
}

/**
 * Canonicalize the scan root: unify separators and drop a trailing one.
 *
 * The trailing-separator strip is what makes `relativeToRoot` a plain slice,
 * but it must not eat a filesystem root — `/` and `C:\` name a directory,
 * while `` and `C:` do not. Those two are returned with the separator intact
 * and `rootPrefixLength` accounts for the difference.
 */
export function normalizeRoot(root: string, sep: PathSeparator = pathSeparatorFor(root)): string {
  const unified = sep === "\\" ? root.replace(/\//g, "\\") : root;
  const stripped = unified.replace(/[\\/]$/, "");
  // Bare `/` or a bare drive (`C:`) is not a usable directory — keep the
  // separator that made it one.
  if (stripped === "" || /^[A-Za-z]:$/.test(stripped)) return unified;
  return stripped;
}

/** Join a directory and an entry name with the directory's own separator. */
export function joinPath(dir: string, name: string, sep: PathSeparator): string {
  return dir.endsWith(sep) ? `${dir}${name}` : `${dir}${sep}${name}`;
}

/**
 * How many leading characters of a descendant path belong to the root.
 *
 * A normalized root usually has no trailing separator, so the separator that
 * `joinPath` inserted is the extra `+ 1`. A filesystem root kept its trailing
 * separator, so there is nothing extra to skip.
 */
export function rootPrefixLength(normalizedRoot: string, sep: PathSeparator): number {
  return normalizedRoot.endsWith(sep) ? normalizedRoot.length : normalizedRoot.length + 1;
}

/** Path of `fullPath` relative to an already-normalized `normalizedRoot`. */
export function relativeToRoot(
  normalizedRoot: string,
  fullPath: string,
  sep: PathSeparator,
): string {
  return fullPath.slice(rootPrefixLength(normalizedRoot, sep));
}
