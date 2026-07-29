/**
 * Path-equality helpers for the terminal surface.
 *
 * Every path that reaches the frontend is written by a different producer with
 * its own spelling conventions: `TerminalTab.workingDir` carries the spawn cwd
 * verbatim (`D:\qontinui-root\qontinui-runner`), a saved project root comes
 * from an OS folder picker (`D:/qontinui-root/qontinui-runner/`), and a
 * hand-typed `cd` can arrive in either form. Comparing them raw produces
 * silent false mismatches, which on the project surface means telling the
 * operator that terminals are "in a different folder" when they are not.
 *
 * The CANONICAL normalizer for this project is Rust's
 * `src-tauri/src/commands/saved_projects.rs::normalize_path` — it is the one
 * the server-side prefix joins (`ProjectSnapshot`) run through, and per the
 * projects-dashboard plan §4 it is being extended with full case folding, MSYS
 * `/d/` rewriting and `subst` alias resolution. This TS helper deliberately
 * implements only the subset that is reproducible in the webview (separator
 * direction, trailing separators, case) and MUST NOT grow a second, divergent
 * alias model: when the Rust side gains one, expose it as a command and delete
 * the local fold rather than mirroring it here.
 *
 * Case folding is unconditional (not gated on `navigator.platform`) so the
 * behavior is identical in the Tauri webview and in the node test
 * environment. The runner is a Windows-first desktop app; folding a
 * case-sensitive Unix path here can only ever collapse two paths that differ
 * by case, which is not a distinction any of these producers relies on.
 */

/**
 * Fold a path into its comparison form: `\` → `/`, trailing separators
 * dropped (never to the empty string — a bare `"/"` stays `"/"`), lowercased.
 */
export function normalizePathForCompare(p: string): string {
  const slashed = p.replace(/\\/g, "/");
  // `(.)` keeps at least one character, so "/" and "//" fold to "/" rather
  // than to "" (which would compare equal to every falsy path).
  return slashed.replace(/(.)\/+$/, "$1").toLowerCase();
}

/**
 * True when two paths name the same directory under
 * {@link normalizePathForCompare}. Empty / undefined on either side is `false`
 * — an unknown path is never "the same" as a known one.
 */
export function samePath(a: string | undefined | null, b: string | undefined | null): boolean {
  if (!a || !b) return false;
  return normalizePathForCompare(a) === normalizePathForCompare(b);
}
