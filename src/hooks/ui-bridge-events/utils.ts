import { getApiPort } from "@/lib/runner-api";
import {
  type RegisteredElement,
  type RegisteredComponent,
  serializeRegisteredElement,
} from "@qontinui/ui-bridge";
import type { SerializedElement, SerializedComponent } from "./types";

/**
 * Send a response to the Rust backend via HTTP fallback.
 * Used when the Tauri event system is unresponsive.
 */
export async function httpSendResponse(response: unknown): Promise<boolean> {
  try {
    const port = getApiPort();
    const resp = await fetch(`http://localhost:${port}/ui-bridge/ipc-response`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(response),
    });
    return resp.ok;
  } catch {
    return false;
  }
}

/**
 * Send a pong to the Rust backend via HTTP fallback.
 */
export async function httpSendPong(): Promise<boolean> {
  try {
    const port = getApiPort();
    const resp = await fetch(`http://localhost:${port}/ui-bridge/pong`, {
      method: "POST",
    });
    return resp.ok;
  } catch {
    return false;
  }
}

/**
 * Map task run status strings from the runner to SDK-compatible workflow status values.
 */
export function mapTaskRunStatus(status: string): string {
  switch (status) {
    case "in_progress":
    case "running":
      return "running";
    case "completed":
    case "complete":
    case "success":
      return "completed";
    case "failed":
    case "error":
      return "failed";
    case "cancelled":
    case "stopped":
      return "cancelled";
    default:
      return "pending";
  }
}

/**
 * Serialize a RegisteredElement to a plain object (removes DOM references)
 */
export function serializeElement(element: RegisteredElement): SerializedElement {
  // Delegate to ui-bridge's shared serializer so this path can't drift from
  // BridgeSnapshot's element shape. The runner mounts UI Bridge routes under
  // /ui-bridge/control/*, so override the default base path. Add registeredAt
  // /mounted on top — they're single-element-detail-only and not part of the
  // snapshot contract.
  const base = serializeRegisteredElement(element, {
    componentBasePath: "/ui-bridge/control/component",
  });
  return {
    ...base,
    registeredAt: element.registeredAt,
    mounted: element.mounted,
  } as SerializedElement;
}

/**
 * Serialize a RegisteredComponent to a plain object.
 *
 * Phase 3.1 (plan 2026-05-03): forward the `scope` annotation so component
 * listings on the runner carry the same discoverability hint the SDK
 * documents. `undefined` is the documented default for `'route'` (see
 * `RegisteredComponent.scope` JSDoc), so we materialize that default here
 * rather than dropping the key. This guarantees clients can always read
 * `component.scope` without ad-hoc `??= 'route'` plumbing.
 */
export function serializeComponent(component: RegisteredComponent): SerializedComponent {
  return {
    id: component.id,
    name: component.name,
    description: component.description,
    actions: component.actions.map((a) => ({
      id: a.id,
      label: a.label,
      description: a.description,
      paramSchema: a.paramSchema,
      path: `/ui-bridge/control/component/${component.id}/action/${a.id}`,
    })),
    actionInvocationPath: `/ui-bridge/control/component/${component.id}/action/{actionId}`,
    elementIds: component.elementIds,
    registeredAt: component.registeredAt,
    mounted: component.mounted,
    scope: component.scope ?? "route",
  };
}

/**
 * Typed accessor for the global `window.__UI_BRIDGE__` object.
 */
export function getUIBridgeGlobal(): Record<string, unknown> | undefined {
  return (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
    | Record<string, unknown>
    | undefined;
}

/**
 * Derive a short canonical alias for a tab id by stripping a known
 * namespacing prefix (`sm-tab-`). The runner's `MainTabId` catalog
 * doesn't use that prefix, so canonical is typically the id itself —
 * but tabbed sub-pages (state-machine's `sm-tab-graph` / `sm-tab-states`
 * etc.) use a prefix scheme, and exposing the bare segment as
 * `canonical` lets agents reason about the tab uniformly across pages.
 *
 * Used in three places that emit the tab catalog: `tabs_list` and
 * `get_playbook` IPC handlers in `usePageEvents.ts`, and the
 * `availableTabs` enrichment in `useDiscoveryEvents.ts`. Lifting this
 * to a shared helper avoids the three sites drifting apart on what
 * "canonical" means.
 */
export function toTabCanonical(tabId: string): string {
  const prefix = "sm-tab-";
  return tabId.startsWith(prefix) ? tabId.slice(prefix.length) : tabId;
}

/**
 * Compute the Levenshtein edit distance between two strings.
 *
 * Used by the IPC error paths in {@link ./useControlEvents.ts useControlEvents}
 * to suggest closest-match element ids when the caller hits an
 * unknown id (typo recovery). The implementation is a classic
 * iterative DP — O(|a|*|b|) time, O(min(|a|,|b|)) space — and
 * stable enough that we keep it inline rather than pulling in a
 * dependency for ~15 LOC of arithmetic.
 *
 * Returns the minimum number of single-character insertions,
 * deletions, or substitutions to transform `a` into `b`.
 */
export function levenshtein(a: string, b: string): number {
  if (a === b) return 0;
  if (a.length === 0) return b.length;
  if (b.length === 0) return a.length;

  // Always iterate over the shorter string in the inner loop to keep
  // the row buffer small.
  const [shorter, longer] = a.length <= b.length ? [a, b] : [b, a];
  const m = shorter.length;
  const n = longer.length;

  let prev: number[] = new Array(m + 1);
  let curr: number[] = new Array(m + 1);
  for (let j = 0; j <= m; j++) prev[j] = j;

  for (let i = 1; i <= n; i++) {
    curr[0] = i;
    const longerCh = longer.charCodeAt(i - 1);
    for (let j = 1; j <= m; j++) {
      const cost = shorter.charCodeAt(j - 1) === longerCh ? 0 : 1;
      const del = prev[j] + 1;
      const ins = curr[j - 1] + 1;
      const sub = prev[j - 1] + cost;
      curr[j] = del < ins ? (del < sub ? del : sub) : ins < sub ? ins : sub;
    }
    [prev, curr] = [curr, prev];
  }
  return prev[m];
}

/**
 * DEFAULT cap on auto-awaiting a top-level Promise returned by
 * `page_evaluate`. Without a cap, a caller passing
 * `(async () => { await new Promise(() => {}) })()` would wedge the eval
 * response forever. 30 s is generous for legitimate async work (network
 * round-trip, batch DOM scan) while still bounding the worst case.
 *
 * Applies to the legacy IPC `page_evaluate` branch (`usePageEvents.ts`),
 * whose payload carries no timeout. It is also the tagged evaluate handler's
 * fallback when a request omits `timeout_ms` — but a current `page.rs` always
 * emits a number (its own 10 s default when the caller sent none), so on that
 * route this value is reached only from a producer that predates
 * `tagged_page_evaluate`. The DEFAULT a tagged caller actually gets is the
 * Rust one; `timeout_from_default` is what says so. See
 * {@link resolveEvaluateTimeoutMs} and {@link describeEvaluateBudget}.
 */
export const PAGE_EVALUATE_PROMISE_TIMEOUT_MS = 30_000;

/**
 * Ceiling for a caller-supplied `timeout_ms` on the tagged
 * `/control/page/evaluate` flow. Mirrors the Rust-side clamp in
 * `page.rs::ui_bridge_page_evaluate_handler`
 * (`request.timeout_ms.map(|ms| ms.clamp(1000, 600_000))`) so the two ends
 * of the request can't disagree about the maximum. Re-clamped here rather
 * than trusted: the frontend must never accept an unbounded await from a
 * payload, whatever produced it.
 */
export const PAGE_EVALUATE_MAX_TIMEOUT_MS = 600_000;

/**
 * Lower end of the clamp the Rust handler applies to `timeoutMs`
 * (`page.rs`, `.clamp(1000, 600_000)`). Mirrored here only so the timeout
 * message can quote the accepted range instead of leaving a caller to guess.
 */
export const PAGE_EVALUATE_MIN_TIMEOUT_MS = 1_000;

/**
 * Head start the frontend takes over the Rust dispatcher's identical wait.
 *
 * Both ends now time the same request out at the same budget, so without a
 * margin they race and the caller gets whichever message won — either the
 * frontend's precise "Promise did not resolve within Xs" or the Rust side's
 * generic "UI Bridge page_evaluate timed out after Xms", varying run to run.
 * Giving up 250 ms early makes the frontend win deterministically, so the
 * caller always learns WHY (an unsettled Promise) rather than just that
 * something took too long. It is negligible against the 1000 ms floor the
 * Rust handler clamps to, and invisible at the 600 s ceiling.
 */
const PAGE_EVALUATE_TIMEOUT_MARGIN_MS = 250;

/**
 * Resolve the promise-await budget for one tagged evaluate request.
 *
 * The Rust dispatcher forwards the caller's `timeoutMs` (already clamped to
 * [1000, 600000]) on every `ui-bridge:evaluate-request` and waits exactly
 * that long for the response. The frontend used to ignore the field and
 * always await {@link PAGE_EVALUATE_PROMISE_TIMEOUT_MS}, which made the
 * documented ceiling unreachable: a caller asking for `timeoutMs: 600000`
 * to cover a genuinely long async expression got
 * `success: false, error: "…did not settle within 30s"` at 30 s, while the
 * Rust side was still willing to wait another nine and a half minutes.
 * Honoring the field lines both waits up with the caller's request, less
 * {@link PAGE_EVALUATE_TIMEOUT_MARGIN_MS}.
 *
 * Anything not a positive finite number (absent, `null`, `0`, `NaN`, a
 * string from a hand-rolled emit) falls back to the default cap rather than
 * producing an instant or infinite timeout.
 */
export function resolveEvaluateTimeoutMs(raw: unknown): number {
  return describeEvaluateBudget(raw).awaitMs;
}

/**
 * The budget behind one evaluate request, with the provenance a timeout error
 * needs in order to be actionable.
 */
export interface EvaluateBudget {
  /** How long to actually await — the requested budget less the margin. */
  awaitMs: number;
  /** The budget the request is being held to, BEFORE the margin. */
  requestedMs: number;
  /**
   * True when the caller sent no usable `timeoutMs` and this is the default.
   * The difference is the whole point of the timeout message: a default the
   * caller never chose reads as an undocumented cap, and the fix is to say so.
   *
   * "Default" means *nobody chose it*, which is NOT the same as "this payload
   * carried no number" — the Rust dispatcher substitutes its own default
   * before emitting, so on the tagged route a defaulted budget arrives as a
   * concrete number. {@link describeEvaluateBudget}'s `rawIsDefault` option is
   * how that provenance survives the seam.
   */
  fromDefault: boolean;
}

/**
 * Out-of-band provenance for a `timeoutMs` that has already been defaulted by
 * the time it reaches this module.
 */
export interface EvaluateBudgetOptions {
  /**
   * True when `raw` is a number the CALLER did not choose — the Rust
   * dispatcher's `DEFAULT_PAGE_EVALUATE_TIMEOUT_MS` substituted into
   * `timeout_ms` before the emit (`page.rs::tagged_page_evaluate`). Carried on
   * the request as `timeout_from_default`. Absent/false → the number is the
   * caller's own, which is the correct reading for any producer that predates
   * the flag.
   */
  rawIsDefault?: boolean;
}

/**
 * Resolve a raw `timeoutMs` into the await budget AND where that budget came
 * from — the single source of truth {@link resolveEvaluateTimeoutMs} and the
 * timeout message both read.
 *
 * Split out because the message could not previously be honest. It reported
 * only the DERIVED number (`"9.8s"`), which is the default 10 000 ms less the
 * 250 ms margin — so a caller who had never heard of `timeoutMs` saw an
 * arbitrary 9.8 s and reasonably read it as a hard cap on `page_evaluate`.
 * It is neither hard nor a cap: it is this request's own default budget, and
 * the field to raise it already exists.
 *
 * `opts.rawIsDefault` exists because the absence of a number is NOT how a
 * defaulted budget reaches this function on the tagged route. `page.rs`
 * substitutes `DEFAULT_PAGE_EVALUATE_TIMEOUT_MS` (10 s) before it emits, so
 * `raw` is always a concrete number there — and inferring provenance from
 * `raw` alone re-created the very defect this function was split out to fix,
 * one seam further along: a caller who sent nothing was told "That budget came
 * from the `timeoutMs` you sent". `page/evaluate-raw` made that unarguable —
 * it has no `timeoutMs` field for a caller to send, and still got that
 * sentence.
 */
export function describeEvaluateBudget(
  raw: unknown,
  opts?: EvaluateBudgetOptions,
): EvaluateBudget {
  if (typeof raw !== "number" || !Number.isFinite(raw) || raw <= 0) {
    return {
      awaitMs: PAGE_EVALUATE_PROMISE_TIMEOUT_MS,
      requestedMs: PAGE_EVALUATE_PROMISE_TIMEOUT_MS,
      fromDefault: true,
    };
  }
  const requestedMs = Math.min(raw, PAGE_EVALUATE_MAX_TIMEOUT_MS);
  // Never let the margin push a small budget to zero (or negative), which
  // would make every await fail instantly.
  const awaitMs =
    requestedMs > PAGE_EVALUATE_TIMEOUT_MARGIN_MS
      ? requestedMs - PAGE_EVALUATE_TIMEOUT_MARGIN_MS
      : requestedMs;
  return { awaitMs, requestedMs, fromDefault: opts?.rawIsDefault === true };
}

/**
 * SECURITY: structural code-injection blocks for `page_evaluate` — ALWAYS
 * enforced, whatever the caller passes.
 *
 * Blocks code injection, prototype pollution, cookie exfiltration, redirects,
 * reloads and crypto key access. Deliberately ALLOWS:
 *   - `localStorage` / `sessionStorage` (needed for tab navigation)
 *   - `globalThis` / `Reflect` / `Proxy` (useful for deep inspection)
 *   - `window.location` READS (pathname, host, port, href, origin, protocol,
 *     search, hash, hostname) for test probes — only mutating methods and
 *     assignment are blocked
 *
 * `location.reload` is blocked for the same reason the `page_refresh` handler
 * refuses to reload: a full page reload tears down all React state (auth,
 * execution, the terminal tree), orphaning every live PTY session.
 *
 * These live here, beside {@link compileEvaluateExpression}, because BOTH
 * `page_evaluate` entry points gate on them — the legacy IPC branch
 * (`usePageEvents.ts`) and the tagged-event handler
 * (`useUIBridgeEvaluateHandler.ts`). They used to be two hand-maintained
 * copies under a "any change here MUST be mirrored there" comment, which is a
 * drift hazard with a security blast radius: a pattern added to one gate would
 * leave the other route wide open, and nothing would fail. One array,
 * structurally impossible to half-update.
 */
export const PAGE_EVALUATE_STRUCTURAL_PATTERNS: readonly RegExp[] = [
  /\bimport\s*\(/, // dynamic import — can load arbitrary modules
  /\brequire\s*\(/, // require — same
  /\b__proto__\b/, // prototype pollution
  /\bconstructor\s*\[/, // constructor bracket access
  /\beval\s*\(/, // nested eval — full code execution
  /\bnew\s+Function\b/, // Function constructor — same
  /\bdocument\.cookie\b/, // cookie theft
  /\bwindow\.open\b/, // popup/phishing
  /\bwindow\.location\.(assign|replace|reload)\b/, // redirect / reload
  /\bwindow\.location\s*=/, // redirect via assignment
  /\blocation\.(assign|replace|reload)\b/, // bare redirect / reload
  /\bhistory\.go\s*\(\s*0?\s*\)/, // history.go(0) / go() — reload synonyms; go(±n) stays allowed
  /\blocation\s*=\s*["'`]/, // redirect via bare assignment
  /\bcrypto\.subtle\b/, // key material access
];

/**
 * Network-related blocks for `page_evaluate` — relaxed ONLY when the caller
 * explicitly passes `allowNetworkRequests: true`, so test assertions can hit
 * runner APIs directly without the brittle `window["fet"+"ch"]` workaround.
 * Default is to block them as data-exfiltration / persistent-channel risks.
 */
export const PAGE_EVALUATE_NETWORK_PATTERNS: readonly RegExp[] = [
  /\bfetch\s*\(/, // network requests — data exfiltration
  /\bXMLHttpRequest\b/, // network requests — same
  /\bnavigator\.sendBeacon\b/, // data exfiltration beacon
  /\bWebSocket\b/, // persistent network channel
];

/**
 * Pure gate for `page_evaluate` expressions. Returns the first prohibited
 * pattern the expression matches, or `null` when the expression is allowed.
 *
 * The returned `RegExp` is shared module state, but every pattern above is
 * flag-free, so `.test()` keeps no `lastIndex` cursor and concurrent callers
 * cannot interfere with each other.
 */
export function checkEvaluateBlocklist(
  expression: string,
  allowNetworkRequests: boolean,
): RegExp | null {
  const patterns = allowNetworkRequests
    ? PAGE_EVALUATE_STRUCTURAL_PATTERNS
    : [...PAGE_EVALUATE_STRUCTURAL_PATTERNS, ...PAGE_EVALUATE_NETWORK_PATTERNS];
  return patterns.find((p) => p.test(expression)) ?? null;
}

/**
 * Skip over a quoted run (`'`, `"` or a template literal) starting at
 * `start`, returning the index of its closing quote, or -1 if unterminated.
 *
 * Template literals recurse through `${…}` interpolations (which can contain
 * further strings and templates) so a `;` or newline buried in one is never
 * mistaken for a statement break.
 */
function skipQuotedRun(source: string, start: number): number {
  const quote = source[start];
  for (let i = start + 1; i < source.length; i++) {
    const ch = source[i];
    if (ch === "\\") {
      i++;
      continue;
    }
    if (ch === quote) return i;
    if (quote === "`" && ch === "$" && source[i + 1] === "{") {
      let depth = 1;
      let j = i + 2;
      for (; j < source.length && depth > 0; j++) {
        const c = source[j];
        if (c === "\\") {
          j++;
        } else if (c === '"' || c === "'" || c === "`") {
          const end = skipQuotedRun(source, j);
          if (end === -1) return -1;
          j = end;
        } else if (c === "{") {
          depth++;
        } else if (c === "}") {
          depth--;
        }
      }
      if (depth > 0) return -1;
      i = j - 1;
    }
  }
  return -1;
}

/**
 * Indices of every TOP-LEVEL statement break (`;` or newline) in `source` —
 * top-level meaning outside all brackets, strings, template interpolations
 * and comments.
 *
 * Deliberately a scanner and not a parser. Its only consumer is
 * {@link splitTrailingStatement}, whose output is handed to `new Function`
 * for validation, so a mis-scan (an unrecognised regex literal containing a
 * `;`, say) produces a body that fails to compile and falls through to the
 * next arm. Wrong-but-compiling is not reachable: the rewrite only ever
 * moves a `return (` in front of a trailing fragment of the SAME source, so
 * if it compiles, it runs the same statements in the same order.
 */
function topLevelStatementBreaks(source: string): number[] {
  const breaks: number[] = [];
  let depth = 0;
  for (let i = 0; i < source.length; i++) {
    const ch = source[i];
    const next = source[i + 1];
    if (ch === "/" && next === "/") {
      const nl = source.indexOf("\n", i + 2);
      if (nl === -1) break;
      i = nl - 1;
      continue;
    }
    if (ch === "/" && next === "*") {
      const end = source.indexOf("*/", i + 2);
      if (end === -1) break;
      i = end + 1;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === "`") {
      const end = skipQuotedRun(source, i);
      if (end === -1) return [];
      i = end;
      continue;
    }
    if (ch === "(" || ch === "[" || ch === "{") depth++;
    else if (ch === ")" || ch === "]" || ch === "}") depth--;
    else if (depth === 0 && (ch === ";" || ch === "\n")) breaks.push(i);
  }
  return breaks;
}

/**
 * Completion-value rewrite for a statement list: `var q = 1 + 1; q` becomes
 * `var q = 1 + 1;\nreturn ( q\n)`.
 *
 * A function body has no completion value — `new Function("var q=1+1; q")()`
 * is `undefined` — so statement-style input whose author ended on a bare
 * expression used to come back as `success: true` with nothing in it. That
 * is the same silent false green the paren wrap fixes for leading newlines,
 * reached from the statement side instead.
 *
 * Returns zero or one candidate body: the split at the LAST top-level
 * statement break with a non-blank tail. Zero when there is no top-level
 * break, or nothing but blanks after the last one (`1 + 1;`, already handled
 * by arm 2).
 */
function splitTrailingStatement(expression: string): string[] {
  const breaks = topLevelStatementBreaks(expression);
  for (let i = breaks.length - 1; i >= 0; i--) {
    const at = breaks[i];
    // A trailing `;` (and any trailing blanks) can't ride inside the paren
    // wrap — `return (q;)` is a SyntaxError — so `var q = 1 + 1; q;` would
    // otherwise fall through to arm 4 while its semicolon-less twin worked.
    const tail = expression.slice(at + 1).replace(/[;\s]+$/, "");
    if (tail.length === 0) continue;
    return [expression.slice(0, at + 1) + "\nreturn (" + tail + "\n)"];
  }
  return [];
}

/**
 * Compile a caller-supplied `page_evaluate` expression into a zero-arg
 * function, WITHOUT executing it. Used by both `page_evaluate` call sites
 * (`usePageEvents.ts` legacy IPC branch, `useUIBridgeEvaluateHandler.ts`
 * tagged-event handler) so the wrapping rules can never drift between them.
 *
 * Why the parenthesised wrap (and why this is a bug fix, not a refactor):
 *
 * The naive wrap `new Function("return " + expression)` is broken for any
 * expression that STARTS WITH A NEWLINE — the body becomes
 *
 * ```js
 * return
 * ({a:1})
 * ```
 *
 * and automatic semicolon insertion terminates the `return` on the first
 * line, so the function returns `undefined`. Critically it does NOT throw
 * a SyntaxError, so the statement-style fallback below never fires and the
 * caller gets `success: true` with an empty result — a SILENT FALSE GREEN
 * that corrupts test evidence rather than merely losing it. Any driver
 * that writes a multi-line expression the natural way (heredoc,
 * triple-quoted string, template literal opening on the next line) hits it.
 *
 * Wrapping in parentheses makes the leading newline harmless (a newline
 * inside a parenthesised expression is just whitespace) and, as a bonus,
 * also fixes a leading line comment (`// note\nfoo()`). The `\n` before the
 * closing paren guards the mirror case: a TRAILING line comment would
 * otherwise swallow the `)`.
 *
 * Four compile attempts, most-specific first:
 *
 * 1. `return (<expr>\n)` — parenthesisable expressions. Newline- and
 *    comment-safe at both ends. Covers the overwhelming majority.
 * 2. `return <expr>` with leading whitespace stripped — expressions that
 *    are valid after `return` but not inside parens, chiefly ones ending
 *    in a semicolon (`document.title;`). `trimStart()` is what keeps the
 *    ASI bug from sneaking back in through this arm for a leading-newline
 *    expression that also ends in `;`. SKIPPED ENTIRELY whenever the
 *    expression has a top-level statement break with a non-blank tail —
 *    see below.
 * 3. Statement list whose LAST top-level fragment is returned — the
 *    completion-value arm. `var q = 1 + 1; q` is a statement list (arms 1
 *    and 2 are both SyntaxErrors) whose author plainly means "give me
 *    `q`", but a raw function body has no completion value, so arm 4
 *    returned `undefined` under `success: true` — the same silent false
 *    green the paren wrap exists to prevent, just reached from the other
 *    side. See {@link splitTrailingStatement}.
 * 4. The raw expression as a function body — genuine statement-style input
 *    with top-level `let`/`const` and an explicit `return`
 *    (`let x = 1; return x + 1`), which is not parenthesisable at all.
 *
 * Arm 3 is a GUARDED transform, and the guard is the load-bearing part: the
 * rewrite is COMPILED first and kept only if it parses. Every shape the
 * scanner can misread — a trailing block statement (`if(x){f()}`, where
 * `return (…)` is a hard SyntaxError), a `;` inside a regex literal (the one
 * literal kind the scanner does not track), a trailing fragment that is a
 * statement rather than an expression (`let x = 1; return x + 1`) — fails to
 * compile and falls through to arm 4, yielding `undefined`. A silently WRONG
 * value would be far worse than the bug this closes, so the transform is
 * never trusted unparsed.
 *
 * WHY ARM 2 IS SKIPPED WHENEVER ARM 3 HAS A CANDIDATE.
 *
 * Arm 2 is `return <statements>`, which for a statement LIST returns the value
 * of the FIRST statement and silently discards the rest: `new Function("return
 * 1+1; 2+2")()` is `2`, not `4`. That compiles, so it wins before arm 3 is ever
 * reached — the wrong-value bug this ordering exists to close. Measured on the
 * unguarded build: `1+1; 2+2`→2, `10; 20; 30`→10, `({a:1}); 7`→`{a:1}`,
 * `function f(){return 9}; f()`→the function object. `var`-prefixed probes mask
 * it completely (`return var q = …` is a SyntaxError), which is why it survived
 * the arm-3 landing.
 *
 * So the skip is UNCONDITIONAL on {@link splitTrailingStatement} producing a
 * candidate, not conditional on that candidate compiling. Merely reordering the
 * arms would not do: `f(); if(x){g()}` has an arm-3 candidate that does NOT
 * compile, so arm 2 would still catch it and return `f()`'s value. Skipping
 * outright sends it to arm 4 and `undefined` instead — losing the value rather
 * than reporting a wrong one.
 *
 * Nothing legitimate is lost. Arm 2's real job is the semicolon-terminated
 * single expression (`document.title;`), and for those the tail after the last
 * top-level break is blank, so `splitTrailingStatement` returns nothing and
 * arm 2 stays in the list.
 *
 * Compilation is deliberately separated from invocation. The previous code
 * compiled-and-called inside one `try`, so an expression that merely THREW
 * a `SyntaxError` at runtime (e.g. `JSON.parse("{")`) was misread as a
 * failed wrap and re-executed as a raw body — a second execution of an
 * expression the evaluate handler guarantees runs exactly once. Here a
 * runtime throw of any type propagates to the caller's error envelope.
 */
export function compileEvaluateExpression(expression: string): () => unknown {
  const trailing = splitTrailingStatement(expression);
  const candidates =
    trailing.length > 0
      ? // Statement list: arm 2 would return the FIRST statement's value.
        ["return (" + expression + "\n)", ...trailing, expression]
      : ["return (" + expression + "\n)", "return " + expression.trimStart(), expression];
  let lastSyntaxError: SyntaxError | undefined;
  for (const body of candidates) {
    try {
      return new Function(body) as () => unknown;
    } catch (err) {
      if (err instanceof SyntaxError) {
        lastSyntaxError = err;
        continue;
      }
      throw err;
    }
  }
  throw lastSyntaxError ?? new SyntaxError("page_evaluate: expression failed to compile");
}

/**
 * The discriminated `{value, type}` shape returned by `page_evaluate` when the
 * caller passes `unwrap: true`.
 */
export interface UnwrappedEvaluateResult {
  value: unknown;
  type: "scalar" | "object" | "undefined" | "function" | "null";
}

/**
 * Shape an evaluated result into the opt-in discriminated `{value, type}`
 * envelope. Shared by both `page_evaluate` entry points so the two routes
 * cannot report different `type` discriminants for the same value.
 *
 * Functions aren't structured-clone-safe, so a function result surfaces its
 * name (or `<anonymous>`) rather than failing the IPC hop.
 */
export function describeEvaluateResult(resolved: unknown): UnwrappedEvaluateResult {
  if (resolved === null) return { value: null, type: "null" };
  if (resolved === undefined) return { value: undefined, type: "undefined" };
  if (typeof resolved === "function") {
    return { value: (resolved as { name?: string }).name || "<anonymous>", type: "function" };
  }
  if (typeof resolved === "object") return { value: resolved, type: "object" };
  return { value: resolved, type: "scalar" };
}

/**
 * Duck-typed thenable check. Spec-correct (matches what `await` itself
 * does): "thenable" is "has a callable `.then`". Catches cross-realm
 * Promises (e.g. iframes whose Promise constructor lives in a different
 * realm) that `instanceof Promise` would miss.
 */
export function isThenable(value: unknown): value is PromiseLike<unknown> {
  return (
    value !== null &&
    (typeof value === "object" || typeof value === "function") &&
    typeof (value as { then?: unknown }).then === "function"
  );
}

/**
 * If `value` is a thenable, await it with a timeout cap; otherwise
 * return as-is (no Promise-wrap allocation for the common synchronous
 * case). Used by the `page_evaluate` handlers to auto-resolve top-level
 * Promises returned by expressions like `(async () => ({a:1}))()` so
 * callers don't have to spell out `.then(v => v)` to unwrap them.
 *
 * On timeout, throws an Error whose message names the elapsed seconds —
 * the caller's existing try/catch maps that to the standard error
 * envelope, so timeouts are surfaced as `success: false, error: "..."`
 * rather than wedging the response forever. Rejections of the awaited
 * Promise propagate normally (same try/catch handles them).
 */
/**
 * Build the `page_evaluate` timeout message.
 *
 * Reports the REQUESTED budget rather than the derived await, names where that
 * budget came from, and points at the knob — because the previous message did
 * none of the three. `"Promise did not resolve within 9.8s"` is the default
 * 10 000 ms minus a 250 ms reporting margin, and it read to callers as a hard
 * ceiling on `page_evaluate` that no field could raise. The field exists, the
 * Rust handler honours it, and the ceiling is 600 s.
 *
 * `budget` is optional so a caller with only a bare number still gets the old
 * (honest, if terser) sentence rather than a fabricated provenance.
 *
 * The leading clause stays verbatim `"Promise did not resolve within Xs"`: it
 * is the documented discriminator between a frontend timeout and the Rust
 * side's generic `"UI Bridge page_evaluate timed out after Xms"` (see
 * {@link PAGE_EVALUATE_TIMEOUT_MARGIN_MS}). Only the number it quotes changed
 * — from the derived await to the requested budget — plus everything after it.
 */
export function evaluateTimeoutMessage(timeoutMs: number, budget?: EvaluateBudget): string {
  const secs = (ms: number) => `${(ms / 1000).toFixed(1)}s`;
  if (!budget) {
    return `page_evaluate: Promise did not resolve within ${secs(timeoutMs)}`;
  }
  const source = budget.fromDefault
    ? `That is the DEFAULT budget, not a cap — pass \`timeoutMs\` to raise it`
    : `That budget came from the \`timeoutMs\` you sent`;
  return (
    `page_evaluate: Promise did not resolve within ${secs(budget.requestedMs)} ` +
    `(awaited ${secs(timeoutMs)}; ${PAGE_EVALUATE_TIMEOUT_MARGIN_MS}ms is reserved so this ` +
    `error can be reported instead of the call being cut off). ` +
    `${source} (clamped to ${PAGE_EVALUATE_MIN_TIMEOUT_MS}-${PAGE_EVALUATE_MAX_TIMEOUT_MS}ms).`
  );
}

export async function awaitWithTimeout(
  value: unknown,
  timeoutMs: number,
  budget?: EvaluateBudget,
): Promise<unknown> {
  if (!isThenable(value)) {
    return value;
  }
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeoutPromise = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      reject(new Error(evaluateTimeoutMessage(timeoutMs, budget)));
    }, timeoutMs);
  });
  try {
    return await Promise.race([value, timeoutPromise]);
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}

/**
 * Pick up to 5 element ids closest to `target` by Levenshtein distance,
 * filtered to ids whose distance is at most `floor(target.length / 2)`.
 *
 * This is the closest-match scaffolding used by the element-not-found
 * IPC error path to surface typo-recovery suggestions in the response
 * `hint.closestMatches` field. The threshold cap prevents `"foo"` from
 * matching every 4+ char id in the registry — short queries get tight
 * thresholds so we only return genuinely similar ids.
 */
export function closestElementIds(target: string, candidates: readonly string[]): string[] {
  if (!target) return [];
  const threshold = Math.floor(target.length / 2);
  const scored: Array<{ id: string; distance: number }> = [];
  for (const id of candidates) {
    if (id === target) continue;
    const distance = levenshtein(target, id);
    if (distance <= threshold) {
      scored.push({ id, distance });
    }
  }
  scored.sort((a, b) => a.distance - b.distance || a.id.localeCompare(b.id));
  return scored.slice(0, 5).map((s) => s.id);
}

/**
 * Per-element action-allow gate for the `execute_action` IPC handler.
 *
 * Returns `true` when `action` may be dispatched to the element. An element
 * with NO declared action set (`allowedActions` empty) is permissive (the SDK's
 * global supported-action validation still applies downstream). When a declared
 * set is present, the action must be in it — EXCEPT `hoverClick`, a click-variant
 * (reveal a `pointer-events:none`-until-`:hover`/`group-hover` control, then
 * click) that is allowed wherever `click` is. This mirrors the runner-side Rust
 * `is_action_advertised` click-variant exemption so the two advertised-action
 * gates (Rust pre-IPC + this frontend pre-check) can't disagree and silently
 * reject a hover-gated toolbar button registered with an explicit `actions=` prop.
 */
export function isElementActionAllowed(
  allowedActions: readonly string[],
  action: string,
): boolean {
  if (allowedActions.length === 0) return true;
  if (allowedActions.includes(action)) return true;
  if (action === "hoverClick" && allowedActions.includes("click")) return true;
  return false;
}
