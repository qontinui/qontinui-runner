import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './playwright-user-scripts',
  testMatch: '**/*.spec.ts',
  timeout: 60000,
  use: {
    // Capture screenshot after every action for debugging
    screenshot: 'on',
    // Always capture trace for debugging
    trace: 'on',
    // Capture video for all tests
    video: 'on',
    // Slow down actions for visibility in headed mode
    actionTimeout: 10000,
  },
  // Retry once to capture more info
  retries: 1,
  // More verbose reporter
  reporter: [
    ['json', { outputFile: 'test-results/results.json' }],
    ['line'],
  ],
});
