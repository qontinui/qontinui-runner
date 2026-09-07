/**
 * terminalScrollbackParams — the `maxLines` half of what `terminalTextPayload.ts`
 * does for `text` and `terminalKeySequence.ts` does for `keys`: validate an
 * automation payload at the handler boundary, and fail with a machine-readable
 * code rather than letting an unchecked value reach arithmetic.
 *
 * ## Why this exists (manual-test-loop iteration 25)
 *
 * `getScrollback` read its bound as
 *
 *     const { maxLines = 500 } = (params || {}) as { maxLines?: number };
 *
 * on BOTH the proxy (`TerminalBridgeProxies.tsx`) and the mounted
 * (`TerminalInstance.tsx`) path — a `number` ASSERTION over an untrusted HTTP
 * body, not a check. A non-number `maxLines` therefore poisoned the arithmetic
 * with `NaN`, and because the two paths slice their buffers by DIFFERENT
 * expressions, the same poisoned value produced OPPOSITE answers:
 *
 *   - proxy:   `lines.slice(Math.max(0, len - maxLines))` → `slice(NaN)` → 0,
 *              so the caller got EVERYTHING;
 *   - mounted: `for (i = Math.max(0, total - maxLines); i < total; i++)`, and
 *              `NaN < total` is `false`, so the caller got NOTHING.
 *
 * Measured 3/3 reps with 40 lines in each pane, both answering HTTP 200
 * `success: true`:
 *
 *   | `maxLines`     | proxy                | mounted        |
 *   |----------------|----------------------|----------------|
 *   | `1`            | 21 chars / 1 line    | 21 chars / 1 line   (agree) |
 *   | `{"a":1}`      | 336 chars / 42 lines | `""` (0 chars) — DIVERGE |
 *   | `"abc"`        | 336 chars / 42 lines | `""` (0 chars) — DIVERGE |
 *
 * A read tool that answers "the whole buffer" on one pane and "nothing" on
 * another, for the identical request, is worse than one that errors: an
 * automation reading `""` concludes the pane is idle. That is the
 * silent-empty-is-unknown trap with a `200` stamped on it. Whether a pane is
 * proxy-backed or mounted is a property of the VIEWPORT — a pane scrolling
 * through a virtualized flow grid crosses that line on its own — so the same
 * script gets different answers at different moments.
 *
 * Also wrong on both paths before this guard: `-5`, `0` and `null` answered
 * `""` with a 200; `"3"` and `[1]` were silently number-coerced by the
 * subtraction; `true` became `1`.
 *
 * ## The call on `maxLines: 0`
 *
 * REJECTED, together with every other non-positive value. `0` is arguably a
 * valid "no lines please" request, and if it were served it would have to
 * answer `""` — which is indistinguishable from an empty pane, a dead pane, and
 * a read that failed. A caller that genuinely wants no scrollback does not need
 * to call `getScrollback` at all, whereas a `0` arriving here is far more often
 * an off-by-one or a `parseInt` that yielded nothing. So the accepted domain is
 * a POSITIVE INTEGER, stated in one sentence, identical on both paths.
 *
 * Absent/`undefined` keeps its documented default of
 * {@link DEFAULT_SCROLLBACK_MAX_LINES} — unchanged behaviour for every caller
 * that never passed the parameter.
 *
 * Leaf module — zero imports — for the same reason `terminalKeySequence.ts`,
 * `terminalTextPayload.ts` and `terminalWriteResult.ts` are: `TerminalInstance`
 * transitively pulls `@xterm/addon-canvas`, which touches `self` at module init
 * and crashes under the runner's `environment: "node"` vitest config.
 */

/** Machine-readable failure code: `getScrollback`'s `maxLines` was not usable. */
export const SCROLLBACK_MAX_LINES_INVALID = "SCROLLBACK_MAX_LINES_INVALID";

/** Lines returned when a caller passes no `maxLines` at all. */
export const DEFAULT_SCROLLBACK_MAX_LINES = 500;

/** Build a typed, coded error in the same shape `requireTextPayload` throws. */
function invalid(detail: string): Error {
  const err = new Error(`${SCROLLBACK_MAX_LINES_INVALID}: ${detail}`) as Error & {
    code?: string;
  };
  err.code = SCROLLBACK_MAX_LINES_INVALID;
  return err;
}

/**
 * Describe a rejected value by TYPE, not by content — same discipline, and same
 * reason, as `terminalTextPayload.ts`'s `describe`.
 */
function describe(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return "an array";
  const kind = typeof value;
  return kind === "object" || kind === "undefined" ? `an ${kind}` : `a ${kind}`;
}

/**
 * Validate a `getScrollback` line bound.
 *
 * @param maxLines The caller-supplied value, untrusted and of unknown type.
 * @returns the bound, narrowed to a positive integer — the default when absent.
 * @throws a coded `Error` for anything else. The buffer is NOT read and no
 * partial answer is returned: an error is the only response that cannot be
 * mistaken for an empty pane.
 */
export function requireMaxLines(maxLines: unknown): number {
  if (maxLines === undefined) return DEFAULT_SCROLLBACK_MAX_LINES;
  if (typeof maxLines !== "number" || !Number.isInteger(maxLines)) {
    throw invalid(
      `getScrollback: 'maxLines' must be a positive integer; received ${describe(maxLines)}. ` +
        "It was NOT coerced: an unchecked value poisoned the slice arithmetic with NaN " +
        "and made the proxy path return the WHOLE buffer where the mounted path " +
        "returned an empty string, both with HTTP 200.",
    );
  }
  if (maxLines < 1) {
    throw invalid(
      `getScrollback: 'maxLines' must be at least 1; received ${maxLines}. ` +
        "A non-positive bound can only answer an empty string, which is " +
        "indistinguishable from an idle pane, a dead pane and a failed read.",
    );
  }
  return maxLines;
}
