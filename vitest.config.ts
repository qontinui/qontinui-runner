import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { resolve } from "path";

export default defineConfig({
  plugins: [react()],
  test: {
    globals: true,
    environment: "node",
    include: ["src/**/*.{test,spec}.{js,mjs,cjs,ts,mts,cts,jsx,tsx}"],
    exclude: ["node_modules", "src-tauri"],
    coverage: {
      provider: "v8",
      reporter: ["text", "json", "html"],
      include: ["src/lib/step-output-handlers/**/*.ts"],
      exclude: ["**/*.test.ts", "**/*.spec.ts", "**/index.ts"],
    },
  },
  resolve: {
    // @qontinui/* TS packages are consumed from npm (same as vite.config.ts —
    // its alias block was removed when the packages moved to npm). The stale
    // sibling-repo aliases that used to live here pointed at
    // ../qontinui-schemas/ts/dist and ../qontinui-workflow-utils/dist, which
    // don't exist on a fresh checkout (or any machine without those siblings
    // built), so every test importing @qontinui/shared-types/* or
    // @qontinui/workflow-utils failed to even load. node_modules has both
    // packages with complete exports maps — no alias needed.
    alias: [{ find: "@", replacement: resolve(__dirname, "./src") }],
  },
});
