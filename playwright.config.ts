// LIVE PRODUCT CONFIG — NOT dead CI scaffolding. Do not delete.
//
// Every local signal says this file is dead: `./playwright-user-scripts` does
// not exist in the source tree, no `*.spec.ts` files exist in this repo, no
// npm script runs Playwright (`pnpm test` is vitest), and no CI workflow
// references Playwright. All four signals are misleading:
//
// - `testDir: "./playwright-user-scripts"` resolves to a RUNTIME directory
//   created by the Rust backend inside the runner's data dir
//   (src-tauri/src/playwright/executor.rs:177,529). The runner writes
//   user-generated Playwright test files there and executes them against the
//   users' own applications — a shipped product feature. The directory is
//   intentionally absent from the source tree.
// - `@playwright/test` is a RUNTIME dependency: executor.rs:165-169 resolves
//   it out of node_modules and hard-errors without it;
//   scripts/headless-launcher.js:42 requires it for the MCP headless-browser
//   path; orchestrator/verification.rs:232 generates `npx playwright test`
//   commands. Do not remove it when pruning devDependencies. (Keep it in
//   devDependencies — moving it to dependencies is a packaging change with
//   its own blast radius, not a cleanup.)
// - This config is NOT used by this repo's CI. The UI-Bridge-not-Playwright
//   rule governs how we test OUR UIs; this is Qontinui running Playwright on
//   behalf of users — a different thing.
//
// Falsification record: qontinui-dev-notes/plans/
//   2026-07-22-runner-playwright-config-looks-dead-but-is-a-live-product-path.md
import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./playwright-user-scripts",
  testMatch: "**/*.spec.ts",
  timeout: 60000,
  use: {
    // Capture screenshot after every action for debugging
    screenshot: "on",
    // Always capture trace for debugging
    trace: "on",
    // Capture video for all tests
    video: "on",
    // Slow down actions for visibility in headed mode
    actionTimeout: 10000,
  },
  // Retry once to capture more info
  retries: 1,
  // More verbose reporter
  reporter: [["json", { outputFile: "test-results/results.json" }], ["line"]],
});
