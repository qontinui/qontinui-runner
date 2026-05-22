#!/usr/bin/env node
/**
 * Headless browser launcher for Spec-CI testing.
 *
 * Launches a Playwright Chromium browser, optionally signs in to a
 * qontinui-web instance via the backend auth API (cookies get set into
 * the browser context automatically), navigates to a target URL, and
 * holds the browser open so the runner-side UI Bridge SDK can connect.
 *
 * Programmatic login (the Bucket C7 piece) uses
 * `context.request.post()` so the response's `Set-Cookie` HttpOnly
 * cookies attach to the same browser context the page runs in. This is
 * the only way to satisfy qontinui-web's auth middleware without
 * filling out the login form — the frontend's `AuthService.login` does
 * the same `fetch(..., { credentials: "include" })` shape and reads the
 * cookies back transparently.
 *
 * Usage:
 *   node headless-launcher.js --url=URL [--headless] [--timeout=30000]
 *                             [--api-base=URL] [--email=X] [--password=Y]
 *
 * Env fallbacks:
 *   QONTINUI_TEST_AUTO_LOGIN_EMAIL    same as --email
 *   QONTINUI_TEST_AUTO_LOGIN_PASSWORD same as --password
 *   QONTINUI_API_BASE_URL             same as --api-base
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
 * Resolve the auth API base URL from CLI, env, or — as a last resort —
 * derive it from the target URL (replace the port with the canonical
 * backend's URL). The derive path covers the "I forgot to pass anything"
 * dev case but warns explicitly so CI never silently hits the wrong env.
 */
function resolveApiBase(targetUrl, cliApiBase) {
  if (cliApiBase) return cliApiBase;
  if (process.env.QONTINUI_API_BASE_URL) return process.env.QONTINUI_API_BASE_URL;

  // Heuristic: if target is localhost:3001 (canonical dev frontend) and no
  // override was given, assume the AWS canonical backend per the project's
  // .env.local convention. Memory: reference_qontinui_web_backend_aws.
  try {
    const t = new URL(targetUrl);
    if (t.hostname === "localhost" || t.hostname === "127.0.0.1") {
      emit({
        status: "api-base-inferred",
        target: targetUrl,
        inferred: "https://api.qontinui.io",
        note: "no --api-base supplied; defaulting to AWS canonical backend",
      });
      return "https://api.qontinui.io";
    }
    // Production / staging — auth API typically lives at the same host
    // with no port change.
    return `${t.protocol}//${t.hostname.replace(/^demo\.staging\./, "api.").replace(/^/, "api.").replace(/^api\.api\./, "api.")}`;
  } catch {
    return null;
  }
}

/**
 * Perform a programmatic sign-in against `/api/v1/auth/jwt/login` using
 * the browser context's request stack so cookies attach to the page's
 * cookie jar. The endpoint expects `application/x-www-form-urlencoded`
 * with `username` + `password` fields (fastapi-users convention).
 *
 * Returns a structured result so the launcher can keep going (and emit
 * a sensible status) even when auth fails — Spec-CI then aborts on the
 * snapshot precondition check, surfacing the same "session expired"
 * error path that catches a stale token in any other context.
 */
async function programmaticLogin(context, apiBase, email, password) {
  if (!apiBase) {
    return { ok: false, reason: "no_api_base" };
  }
  if (!email || !password) {
    return { ok: false, reason: "no_credentials" };
  }

  const url = `${apiBase.replace(/\/$/, "")}/api/v1/auth/jwt/login`;
  const form = new URLSearchParams({ username: email, password }).toString();

  try {
    const resp = await context.request.post(url, {
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      data: form,
    });
    if (!resp.ok()) {
      const body = await resp.text();
      return {
        ok: false,
        reason: `http_${resp.status()}`,
        bodySnippet: body.slice(0, 200),
      };
    }
    return { ok: true };
  } catch (err) {
    return { ok: false, reason: `transport: ${err.message ?? String(err)}` };
  }
}

/**
 * Set the localStorage `is_authenticated` flag the frontend's
 * `TokenStorage` uses to short-circuit re-auth on subsequent loads.
 * Cookies alone are enough for the backend, but the frontend's
 * pre-mount auth check reads localStorage to decide whether to show
 * the LoginScreen before the cookie-backed `/users/me` call resolves.
 *
 * Run this AFTER the first navigation so the page has a window+document
 * to write into.
 */
async function flagAuthenticated(page) {
  await page.evaluate(() => {
    try {
      window.localStorage.setItem("is_authenticated", "true");
    } catch {
      // ignore — some embedded contexts disable localStorage
    }
  });
}

async function main() {
  const args = parseArgs();
  const url = args.url;
  const headless = args.headless !== "false";
  const timeout = parseInt(args.timeout || "30000", 10);
  const apiBase = resolveApiBase(url, args["api-base"]);
  const email = args.email || process.env.QONTINUI_TEST_AUTO_LOGIN_EMAIL;
  const password = args.password || process.env.QONTINUI_TEST_AUTO_LOGIN_PASSWORD;

  if (!url) {
    emit({ status: "error", message: "Missing --url argument" });
    process.exit(1);
  }

  emit({ status: "launching", url, apiBase, headless });

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

    // Sign in BEFORE navigation so the auth cookies attach to the context.
    // If credentials weren't supplied we skip silently — the launcher is
    // still useful for unauthenticated pages (e.g. /login itself, marketing
    // routes, demos).
    const auth = await programmaticLogin(context, apiBase, email, password);
    emit({
      status: "auth-attempted",
      ok: auth.ok,
      reason: auth.reason,
      email: email ? "<redacted>" : null,
    });

    const page = await context.newPage();

    // First navigation may itself be redirected to /login if auth failed
    // or wasn't attempted. We don't fight that here — the consumer (the
    // Spec-CI executor) detects the login-page footprint via its
    // precondition check and aborts loudly.
    await page.goto(url, { waitUntil: "networkidle", timeout });

    // After load, persist the auth-flag the frontend's TokenStorage
    // expects. Harmless if we're on /login already (the flag is read by
    // the post-auth path that's no-op here).
    if (auth.ok) {
      await flagAuthenticated(page);
    }

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
