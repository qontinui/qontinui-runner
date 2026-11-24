/**
 * Loud-failure helpers for source-reading ("wiring") tests.
 *
 * A wiring test that slices source text between two markers is only as strong
 * as its markers. `String.prototype.indexOf` answers `-1` for an absent marker
 * and `slice(start, -1)` then quietly runs to the end of the file, so the
 * guard degrades into a whole-file presence check and stays green. That was
 * qontinui-runner#1831's second vacuous guard. These helpers make the absence a
 * thrown, separately-worded error instead, in the same spirit as
 * `emptyReadArgs()` in `FleetSessionPicker.wiring.test.ts`.
 */

import { fileURLToPath } from "node:url";

/**
 * Index of `marker` in `src` at or after `from`. Throws, naming the marker and
 * the search start, when it is absent — never returns -1.
 *
 * `from` lets an END marker be required to occur AFTER its start marker, which
 * also rules out the second quiet failure: an end marker that matches earlier
 * in the file than the start, whose `slice` is the empty string.
 */
export function assertAnchored(src: string, marker: string, from = 0): number {
  const at = src.indexOf(marker, from);
  if (at < 0) {
    throw new Error(
      `assertAnchored: marker not found${from > 0 ? ` at or after offset ${from}` : ""}: ` +
        JSON.stringify(marker) +
        ` — the source was reworded or reformatted; a slice guard cannot speak for code it can no longer locate`,
    );
  }
  return at;
}

/**
 * Directory a wiring test reads its subject sources from.
 *
 * Normally the test's own directory. The mutation probe
 * (`qontinui-claude-config/scripts/mutation-probe.py`) stages a mutated copy of
 * that directory and exports it as `MP_STAGE`; honouring it is what lets the
 * UNCHANGED test file be rerun against the mutant.
 */
export function subjectDir(testFileUrl: string): string {
  const staged = process.env.MP_STAGE;
  if (staged) {
    // An ambient MP_STAGE (e.g. leaked from an aborted mutation-probe run)
    // silently redirects every guard's reads; make the redirect visible.
    console.warn(`subjectDir: MP_STAGE is set, reading subject sources from ${staged}`);
    return staged.endsWith("/") ? staged : `${staged}/`;
  }
  return fileURLToPath(new URL(".", testFileUrl));
}

/**
 * Index of the character that closes the string literal opened at `code[open]`
 * (a `"`, `'` or backtick), or -1 when it never closes.
 *
 * Backslash escapes are honoured. In a template literal each `${…}` is scanned
 * as CODE — braces counted, nested literals (including nested templates)
 * skipped recursively — so a backtick inside an interpolation cannot end the
 * outer template early. Before this, `` `a ${`b`} c` `` closed at the second
 * backtick and every later quote was read inverted: a scan that silently
 * measured the wrong text instead of throwing.
 */
function closeOfLiteral(code: string, open: number): number {
  const quote = code[open];
  for (let i = open + 1; i < code.length; i += 1) {
    const ch = code[i];
    if (ch === "\\") i += 1;
    else if (ch === quote) return i;
    else if (quote === "`" && ch === "$" && code[i + 1] === "{") {
      let depth = 0;
      let j = i + 1;
      for (; j < code.length; j += 1) {
        const c = code[j];
        if (c === '"' || c === "'" || c === "`") {
          j = closeOfLiteral(code, j);
          if (j < 0) return -1;
        } else if (c === "{") depth += 1;
        else if (c === "}") {
          depth -= 1;
          if (depth === 0) break;
        }
      }
      if (j >= code.length) return -1;
      i = j;
    }
  }
  return -1;
}

/**
 * The full text of every `<needle>…)` call in `code` (comments already
 * stripped by the caller), found by paren counting.
 *
 * This exists because `/useEffect\([^)]*setAppliedQuery/` and
 * `/setServer\([^)]*limit:/` were UNSATISFIABLE for the code shapes they
 * forbid: `[^)]*` cannot cross the `)` of `() =>`, so an effect or functional
 * update written the way this codebase writes them can never match. Measured by
 * mutating the source to contain exactly the forbidden shape (see
 * FleetSessionPicker.wiring.test.mutants.json) — it stayed green. Slicing the
 * call and asserting on its text has no such blind spot.
 *
 * String literals (single, double, backtick; backslash escapes honoured;
 * template `${…}` interpolation scanned as code) are skipped. Regex literals
 * (and JSX text, where an apostrophe opens a "literal") are NOT recognised — a `(`, `)` or quote inside one moves the scan; that
 * fails loudly (unbalanced / unterminated) rather than quietly in the shapes
 * measured so far, and is a stated residual.
 *
 * Throws when `needle` occurs nowhere, so "no call contains X" can never be
 * satisfied by a scan that found no calls, and when a call or a literal inside
 * it never closes.
 */
export function callsOf(code: string, needle: string): string[] {
  const calls: string[] = [];
  let from = 0;
  for (;;) {
    const at = code.indexOf(needle, from);
    if (at < 0) break;
    const open = at + needle.length - 1;
    let depth = 0;
    let end = -1;
    for (let i = open; i < code.length; i += 1) {
      const ch = code[i];
      if (ch === '"' || ch === "'" || ch === "`") {
        // Skip a string literal whole so a paren inside it cannot move the depth.
        const close = closeOfLiteral(code, i);
        if (close < 0) throw new Error(`unterminated string literal in ${needle} call`);
        i = close;
      } else if (ch === "(") depth += 1;
      else if (ch === ")") {
        depth -= 1;
        if (depth === 0) {
          end = i;
          break;
        }
      }
    }
    if (end < 0) throw new Error(`unbalanced ${needle} call`);
    calls.push(code.slice(at, end + 1));
    from = end + 1;
  }
  if (calls.length === 0)
    throw new Error(`no ${needle} call found; a scan of nothing proves nothing`);
  return calls;
}

/**
 * The top-level arguments of one call as `callsOf` returns it
 * (`name(a, b, …)`), whitespace flattened, empty trailing argument dropped.
 *
 * Commas nested in `()`, `[]`, `{}` or inside any string literal do not split.
 * NOT understood: `<…>` type arguments (`new Map<K, V>()` splits at the comma),
 * regex literals, and JSX text — an apostrophe in JSX text reads as an
 * unterminated literal and throws. Callers pin the arity to catch a mis-split.
 */
export function argsOf(call: string): string[] {
  const open = call.indexOf("(");
  if (open < 0 || !call.endsWith(")"))
    throw new Error(`argsOf: not a call: ${JSON.stringify(call)}`);
  const inner = call.slice(open + 1, -1);
  const args: string[] = [];
  let depth = 0;
  let from = 0;
  for (let i = 0; i < inner.length; i += 1) {
    const ch = inner[i];
    if (ch === '"' || ch === "'" || ch === "`") {
      const close = closeOfLiteral(inner, i);
      if (close < 0)
        throw new Error(`argsOf: unterminated string literal in ${JSON.stringify(call)}`);
      i = close;
    } else if (ch === "(" || ch === "[" || ch === "{") depth += 1;
    else if (ch === ")" || ch === "]" || ch === "}") depth -= 1;
    else if (ch === "," && depth === 0) {
      args.push(inner.slice(from, i));
      from = i + 1;
    }
  }
  args.push(inner.slice(from));
  return args.map((a) => a.replace(/\s+/g, " ").trim()).filter((a) => a.length > 0);
}
