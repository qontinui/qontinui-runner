import path from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { version } from "./package.json";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  define: {
    __APP_VERSION__: JSON.stringify(version),
  },

  // Vite options tailored for Tauri development
  clearScreen: false,
  server: {
    host: '0.0.0.0', // Listen on all network interfaces (needed for WSL2 access)
    port: 1420,
    strictPort: true,
    fs: {
      // Allow serving files from sibling directories (ui-bridge, qontinui-schemas)
      allow: ['.', '..'],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    // Tauri uses Chromium on Windows and WebKit on macOS and Linux
    target: process.env.TAURI_PLATFORM == "windows" ? "chrome105" : "safari13",
    // don't minify for debug builds
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    // produce sourcemaps for debug builds
    sourcemap: !!process.env.TAURI_DEBUG,
  },
  resolve: {
    alias: [
      { find: "@", replacement: path.resolve(__dirname, "./src") },
      { find: "@qontinui/schemas", replacement: path.resolve(__dirname, "../qontinui-schemas/generated/typescript") },
      // Explicit subpath aliases for symlinked packages (Vite can't resolve
      // exports maps from within file:-linked sibling packages)
      { find: /^@qontinui\/shared-types\/(.+)$/, replacement: path.resolve(__dirname, "../qontinui-schemas/ts/dist/$1.js") },
      { find: "@qontinui/shared-types", replacement: path.resolve(__dirname, "../qontinui-schemas/ts/dist/index.js") },
      { find: "@qontinui/workflow-utils", replacement: path.resolve(__dirname, "../qontinui-workflow-utils/dist/index.js") },
    ],
  },
});
