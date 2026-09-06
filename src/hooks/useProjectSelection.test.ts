/**
 * The two predicates a failed `get_user_projects` is run through:
 * `isExpectedNoCloudSession` decides whether it is logged at info or pushed to
 * `console.error`, and `isRetryableCredentialRace` decides whether it is
 * retried at all.
 *
 * Why this is worth testing rather than eyeballing: the console-error channel
 * is consumed as a HEALTH SIGNAL (`/ui-bridge/control/console-errors`), so a
 * wrong answer here does not merely pick the wrong log level — it reports a
 * healthy runner as unhealthy. The tier-gate arm of this predicate exists
 * because that regression already happened once.
 *
 * The reason codes come from `commands::auth::BearerReason` (src-tauri,
 * `e81e75ddc`). They are a diagnostic CONTRACT read out of production logs, and
 * this file is the consumer side of it: a silent rename on either side breaks
 * the classification, and these assertions are what fail when it does.
 */

import { describe, expect, it } from "vitest";
import { isExpectedNoCloudSession, isRetryableCredentialRace } from "./useProjectSelection";

/** Exactly how `commands::auth::no_bearer_error` renders a refusal. */
const asRendered = (code: string) =>
  `Not signed in to Qontinui (${code}). Sign in via Settings → Account.`;

/**
 * The three `BearerReason` codes that mean "no usable cloud session".
 *
 * Spelled out here independently of the source list, so changing one takes two
 * deliberate edits — the same two-edit rule the Rust side applies to the codes
 * themselves.
 */
const ORDINARY = [
  "no_cognito_session",
  "cognito_refresh_token_expired",
  "cognito_refresh_failed_no_stored_token",
];

/** The one that is a genuine fault and must keep reaching `console.error`. */
const A_REAL_FAULT = "cognito_access_token_unreadable";

describe("isExpectedNoCloudSession", () => {
  it("treats every ordinary bearer reason as expected", () => {
    for (const code of ORDINARY) {
      expect(isExpectedNoCloudSession(asRendered(code))).toBe(true);
    }
  });

  /**
   * The regression this change closes. Before it, the predicate matched only
   * the tier-gate sentence and "Not authenticated", so the single most common
   * steady state — a runner that is simply not signed in — took the
   * `console.error` branch on every project load.
   */
  it("no_cognito_session is expected, which it was not before", () => {
    expect(isExpectedNoCloudSession(asRendered("no_cognito_session"))).toBe(true);
  });

  /**
   * The half that keeps the predicate honest. A credential store that yields
   * nothing while nothing failed to refresh IS broken, and suppressing it would
   * trade one silent-health bug for another — the console-error channel would
   * stop reporting a fault it is the only reporter of.
   */
  it("a credential-store read fault is NOT suppressed", () => {
    expect(isExpectedNoCloudSession(asRendered(A_REAL_FAULT))).toBe(false);
  });

  it("keeps the two pre-existing arms", () => {
    expect(
      isExpectedNoCloudSession(
        "Tier 0/1 (Local / LocalProvider) — Qontinui account commands are unavailable.",
      ),
    ).toBe(true);
    expect(isExpectedNoCloudSession("Not authenticated. Please log in first.")).toBe(true);
  });

  /**
   * A genuine load failure must still reach `console.error`; a predicate that
   * answered `true` broadly would silence the channel it exists to protect.
   */
  it("does not swallow an unrelated failure", () => {
    for (const msg of [
      "Get projects failed with status 500: upstream unavailable",
      "error decoding response body",
      "",
    ]) {
      expect(isExpectedNoCloudSession(msg)).toBe(false);
    }
  });

  /**
   * The codes are matched as substrings of a rendered sentence, so a code that
   * is a substring of another would classify both. Pinned because
   * `cognito_access_token_unreadable` — the one fault — must not be shadowed by
   * an ordinary code, and the four names are close enough to make that a real
   * hazard rather than a theoretical one.
   */
  it("no ordinary code is a substring of the fault code", () => {
    for (const code of ORDINARY) {
      expect(A_REAL_FAULT.includes(code)).toBe(false);
    }
  });
});

describe("isRetryableCredentialRace", () => {
  /**
   * The gate this replaces was `errorMsg.includes("Not authenticated")`, and
   * that string is unreachable from `get_user_projects`: the command's only two
   * auth refusals are the tier gate's "Qontinui account commands are
   * unavailable" and `no_bearer_error`'s reason-coded sentence. So the retry
   * had stopped firing for the race it exists to cover, while its own comment
   * and App.tsx's project-fetch effect both went on asserting that it still
   * did.
   */
  it("retries the credential-store read race", () => {
    expect(isRetryableCredentialRace(asRendered(A_REAL_FAULT))).toBe(true);
  });

  /**
   * The cost pin, and the reason this is not simply `!isExpectedNoCloudSession`.
   * Retrying an ordinary code would spend 6s of 1s/2s/3s backoff on EVERY
   * project load for a runner that is merely not signed in — the most common
   * configuration there is, and the one whose health reporting the predicate
   * above was just fixed to leave alone.
   */
  it("does not retry any ordinary no-cloud-session state", () => {
    for (const code of ORDINARY) {
      expect(isRetryableCredentialRace(asRendered(code))).toBe(false);
    }
  });

  it("does not retry the tier gate, which no wait can clear", () => {
    expect(
      isRetryableCredentialRace(
        "Tier 0/1 (Local / LocalProvider) — Qontinui account commands are unavailable.",
      ),
    ).toBe(false);
  });

  /**
   * The accident dropped along with the old substring. `AppError::HttpStatusError`
   * renders as `HTTP <status>: <body>` with the body verbatim, so a web-backend
   * 401 whose body carries the phrase was the only thing the old gate could
   * still match — and it would retry a definitive 4xx three times, the exact
   * opposite of what that variant documents itself for.
   */
  it("does not retry a backend response body that echoes the old string", () => {
    expect(isRetryableCredentialRace('HTTP 401: {"detail":"Not authenticated"}')).toBe(false);
    expect(isRetryableCredentialRace("Not authenticated. Please log in first.")).toBe(false);
  });

  it("does not retry an unrelated failure", () => {
    for (const msg of [
      "Get projects failed with status 500: upstream unavailable",
      "error decoding response body",
      "",
    ]) {
      expect(isRetryableCredentialRace(msg)).toBe(false);
    }
  });

  /**
   * The invariant that keeps the pair coherent as the code list grows: every
   * rendered `BearerReason` is either an ordinary state we log quietly or the
   * one fault we retry-then-report, and never both or neither. A fifth code
   * added to only one of the two lists fails here rather than silently becoming
   * un-retried AND console-errored.
   */
  it("the two predicates partition the bearer reason codes", () => {
    for (const code of [...ORDINARY, A_REAL_FAULT]) {
      const rendered = asRendered(code);
      expect(isExpectedNoCloudSession(rendered)).toBe(!isRetryableCredentialRace(rendered));
    }
  });
});
