#!/usr/bin/env node
/**
 * Headless browser launcher for UI Bridge testing.
 *
 * Launches a Playwright Chromium browser, navigates to a URL, and holds
 * the browser open so the target app's UI Bridge SDK can connect to the
 * runner. The browser stays alive until the process is killed.
 *
 * Usage: node headless-launcher.js --url=URL [--headless] [--timeout=30000]
 *
 * Writes JSON status lines to stdout:
 *   {"status":"launching","url":"..."}
 *   {"status":"ready","pid":<number>}
 *   {"status":"error","message":"..."}
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

async function main() {
  const args = parseArgs();
  const url = args.url;
  const headless = args.headless !== "false";
  const timeout = parseInt(args.timeout || "30000", 10);

  if (!url) {
    emit({ status: "error", message: "Missing --url argument" });
    process.exit(1);
  }

  emit({ status: "launching", url });

  try {
    const browser = await chromium.launch({
      headless,
      args: ["--disable-gpu", "--no-sandbox"],
    });

    const context = await browser.newContext({
      viewport: { width: 1280, height: 720 },
      ignoreHTTPSErrors: true,
    });

    const page = await context.newPage();

    // Navigate with timeout
    await page.goto(url, { waitUntil: "networkidle", timeout });

    emit({ status: "ready", pid: process.pid });

    // Keep alive — wait for SIGTERM/SIGINT
    await new Promise((resolve) => {
      process.on("SIGTERM", resolve);
      process.on("SIGINT", resolve);
      process.on("disconnect", resolve);
    });

    await browser.close();
    emit({ status: "closed" });
  } catch (err) {
    emit({ status: "error", message: err.message });
    process.exit(1);
  }
}

main();
