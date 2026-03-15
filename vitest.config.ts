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
    alias: [
      { find: "@", replacement: resolve(__dirname, "./src") },
      {
        find: /^@qontinui\/shared-types\/(.+)$/,
        replacement: resolve(__dirname, "../qontinui-schemas/ts/dist/$1.js"),
      },
      {
        find: "@qontinui/shared-types",
        replacement: resolve(__dirname, "../qontinui-schemas/ts/dist/index.js"),
      },
      {
        find: "@qontinui/workflow-utils",
        replacement: resolve(__dirname, "../qontinui-workflow-utils/dist/index.js"),
      },
    ],
  },
});
