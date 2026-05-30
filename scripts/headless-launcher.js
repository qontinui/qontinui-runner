#!/usr/bin/env node
/**
 * Headless browser launcher for Spec-CI testing.
 *
 * Launches a Playwright Chromium browser, optionally signs in to a
 * qontinui-web instance, navigates to a target URL, and holds the browser
 * open so the runner-side UI Bridge SDK can connect.
 *
 * Authentication (the Bucket C7 piece) is Cognito-only after the legacy-auth
 * teardown (T3): the web backend's local `/api/v1/auth/jwt/login` endpoint was
 * DELETED. We mint a Cognito **id token** via the public CI app-client's
 * USER_PASSWORD_AUTH `InitiateAuth` API (no AWS creds, no client secret), then
 * seed it as the `access_token` cookie on the browser context. The backend
 * routes that cookie to its Cognito verifier by the token's `iss`. There is NO
 * local-password form to fall back to — sign-in is otherwise a redirect to the
 * Cognito hosted UI (an external origin we can't drive headlessly). This mirrors
 * `qontinui-web/frontend/tests/e2e/auth.setup.ts`.
 *
 * Usage:
 *   node headless-launcher.js --url=URL [--headless] [--timeout=30000]
 *                             [--email=X] [--password=Y]
 *
 * Env fallbacks:
 *   QONTINUI_TEST_AUTO_LOGIN_EMAIL     same as --email (a Cognito user)
 *   QONTINUI_TEST_AUTO_LOGIN_PASSWORD  same as --password (a Cognito user)
 *   QONTINUI_COGNITO_CI_CLIENT_ID      override the public CI app-client id
 *   QONTINUI_COGNITO_REGION            override the Cognito region
 *
 * Writes JSON status lines to stdout:
 *   {"status":"launching","url":"..."}
 *   {"status":"auth-attempted","ok":true,"email":"..."}
 *   {"status":"auth-attempted","ok":false,"reason":"..."}
 *   {"status":"ready","pid":<number>}
 *   {"status":"error","message":"..."}
 *
 * The browser stays alive until SIGTERM/SIGINT/disconnect. CI workflows
 * spawn the launcher in the background, run spec-ci against the runner
 * (which now sees the SDK connected from this Chromium), then send
 * SIGTERM to tear down cleanly.
 */

const { chromium } = require("@playwright/test");

// Cognito CI app client — mirrors qontinui-web/frontend/tests/e2e/auth.setup.ts
// and frontend/tests/spec-ci/run-spec-ci.ts. The id is a PUBLIC app-client id
// (USER_PASSWORD_AUTH, no secret), not a secret.
const COGNITO_CI_CLIENT_ID =
  process.env.QONTINUI_COGNITO_CI_CLIENT_ID || "tb0epbojige1900ipu6q80j6b";
const COGNITO_REGION = process.env.QONTINUI_COGNITO_REGION || "us-east-1";

function parseArgs() {
  const args = {};
  for (const arg of process.argv.slice(2)) {
    if (arg.startsWith("--")) {
      const [key, ...rest] = arg.slice(2).split("=");
      args[key] = rest.length > 0 ? rest.join("=") : true;
    }
  }
  return args;
}

function emit(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

/**
 * Mint a Cognito **id token** via the public InitiateAuth API
 * (USER_PASSWORD_AUTH, no secret → unauthenticated POST, no AWS creds needed).
 *
 * Returns a structured result so the launcher can emit a sensible status. On
 * any failure the result is `{ ok: false, reason }` — the caller decides
 * whether to fail loudly (it does: there is no `/jwt/login` fallback anymore).
 */
async function mintCognitoIdToken(email, password) {
  if (!email || !password) {
    return { ok: false, reason: "no_credentials" };
  }

  try {
    const resp = await fetch(
      `https://cognito-idp.${COGNITO_REGION}.amazonaws.com/`,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/x-amz-json-1.1",
          "X-Amz-Target": "AWSCognitoIdentityProviderService.InitiateAuth",
        },
        body: JSON.stringify({
          AuthFlow: "USER_PASSWORD_AUTH",
          ClientId: COGNITO_CI_CLIENT_ID,
          AuthParameters: { USERNAME: email, PASSWORD: password },
        }),
      },
    );
    if (!resp.ok) {
      const body = await resp.text();
      return {
        ok: false,
        reason: `http_${resp.status}`,
        bodySnippet: body.slice(0, 200),
      };
    }
    const body = await resp.json();
    const idToken = body?.AuthenticationResult?.IdToken;
    if (!idToken) {
      return { ok: false, reason: "no_id_token_in_response" };
    }
    return { ok: true, idToken };
  } catch (err) {
    return { ok: false, reason: `transport: ${err.message ?? String(err)}` };
  }
}

async function main() {
  const args = parseArgs();
  const url = args.url;
  const headless = args.headless !== "false";
  const timeout = parseInt(args.timeout || "30000", 10);
  const email = args.email || process.env.QONTINUI_TEST_AUTO_LOGIN_EMAIL;
  const password =
    args.password || process.env.QONTINUI_TEST_AUTO_LOGIN_PASSWORD;

  if (!url) {
    emit({ status: "error", message: "Missing --url argument" });
    process.exit(1);
  }

  emit({ status: "launching", url, headless });

  let browser;
  try {
    browser = await chromium.launch({
      headless,
      args: ["--disable-gpu", "--no-sandbox"],
    });

    const context = await browser.newContext({
      viewport: { width: 1280, height: 720 },
      ignoreHTTPSErrors: true,
    });

    // Mint a Cognito id token. If credentials weren't supplied we skip the
    // mint silently — the launcher is still useful for unauthenticated pages
    // (e.g. /login itself, marketing routes, demos). But if creds WERE supplied
    // and the mint fails, we fail loudly: there is no `/jwt/login` fallback
    // after the Cognito legacy-auth teardown.
    let auth = { ok: false, reason: "no_credentials" };
    if (email && password) {
      auth = await mintCognitoIdToken(email, password);
      if (!auth.ok) {
        throw new Error(
          `Cognito InitiateAuth failed (reason=${auth.reason}` +
            `${auth.bodySnippet ? `, body=${auth.bodySnippet}` : ""}). ` +
            "There is no local /jwt/login fallback after the Cognito " +
            "legacy-auth teardown — check the CI client id, region, and that " +
            "the credentials are a valid Cognito user.",
        );
      }
    }

    // Seed the access_token cookie (backend routes it to the Cognito verifier
    // by `iss`) BEFORE navigation so the page hydrates authenticated. We need
    // an origin for the cookie URL; derive it from the target URL.
    if (auth.ok) {
      const origin = new URL(url).origin;
      await context.addCookies([
        { name: "access_token", value: auth.idToken, url: origin },
      ]);
      // Set the client-side `is_authenticated` flag the route guard reads,
      // before any document loads, via an init script on the context.
      await context.addInitScript(() => {
        try {
          window.localStorage.setItem("is_authenticated", "true");
        } catch {
          // ignore — some embedded contexts disable localStorage
        }
      });
    }

    emit({
      status: "auth-attempted",
      ok: auth.ok,
      reason: auth.reason,
      email: email ? "<redacted>" : null,
    });

    const page = await context.newPage();

    // First navigation may itself be redirected to /login if auth wasn't
    // attempted. We don't fight that here — the consumer (the Spec-CI
    // executor) detects the login-page footprint via its precondition check
    // and aborts loudly.
    await page.goto(url, { waitUntil: "networkidle", timeout });

    emit({ status: "ready", pid: process.pid, finalUrl: page.url() });

    await new Promise((resolve) => {
      process.on("SIGTERM", resolve);
      process.on("SIGINT", resolve);
      process.on("disconnect", resolve);
    });

    await browser.close();
    emit({ status: "closed" });
  } catch (err) {
    emit({ status: "error", message: err.message ?? String(err) });
    if (browser) {
      try {
        await browser.close();
      } catch {
        // best-effort
      }
    }
    process.exit(1);
  }
}

main();
