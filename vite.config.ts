import path from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { version } from "./package.json";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    // Strip `crossorigin` attribute from script/link tags in built HTML.
    // Tauri's custom-protocol asset handler + WebView2's module loader
    // have issues on cold profiles (fresh EBWebView directories).
    {
      name: "strip-crossorigin",
      enforce: "post" as const,
      transformIndexHtml(html: string) {
        return html.replace(/ crossorigin/g, "");
      },
    },
  ],
  define: {
    __APP_VERSION__: JSON.stringify(version),
  },

  // Vite options tailored for Tauri development
  clearScreen: false,
  server: {
    host: '0.0.0.0', // Listen on all network interfaces (needed for WSL2 access)
    port: 1420,
    strictPort: true,
    hmr: false, // Disable hot-reload to prevent UI flashing during code changes
    fs: {
      // Allow serving files from sibling directories (ui-bridge, qontinui-schemas)
      allow: ['.', '..'],
    },
    headers: {
      // Prevent browser caching of linked package modules served via @fs/ paths.
      // Without this, the Tauri webview HTTP cache may serve stale versions of
      // sibling packages (workflow-ui, schemas, ui-bridge) after rebuilds.
      'Cache-Control': 'no-store',
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
      // Force all React imports to the app's single copy (prevents duplicate
      // React instances from symlinked packages with their own node_modules)
      { find: /^react$/, replacement: path.resolve(__dirname, "node_modules/react") },
      { find: /^react-dom$/, replacement: path.resolve(__dirname, "node_modules/react-dom") },
      { find: /^react-dom\/(.+)$/, replacement: path.resolve(__dirname, "node_modules/react-dom/$1") },
      { find: /^react\/(.+)$/, replacement: path.resolve(__dirname, "node_modules/react/$1") },
      { find: "@", replacement: path.resolve(__dirname, "./src") },
      { find: "@qontinui/schemas", replacement: path.resolve(__dirname, "../qontinui-schemas/generated/typescript") },
      // Explicit subpath aliases for symlinked packages (Vite can't resolve
      // exports maps from within file:-linked sibling packages)
      { find: /^@qontinui\/shared-types\/(.+)$/, replacement: path.resolve(__dirname, "../qontinui-schemas/ts/dist/$1.js") },
      { find: "@qontinui/shared-types", replacement: path.resolve(__dirname, "../qontinui-schemas/ts/dist/index.js") },
      { find: "@qontinui/workflow-utils", replacement: path.resolve(__dirname, "../qontinui-workflow-utils/dist/index.js") },
      // @qontinui/ui-bridge subpath imports must resolve to dist/ to match how bare
      // "ui-bridge" resolves, preventing duplicate module instances (singleton split).
      { find: /^@qontinui\/ui-bridge\/(.+)$/, replacement: path.resolve(__dirname, "../ui-bridge/packages/ui-bridge/dist/$1/index.mjs") },
      { find: "@qontinui/ui-bridge", replacement: path.resolve(__dirname, "../ui-bridge/packages/ui-bridge/dist/index.mjs") },
      // ui-bridge-auto resolves to source (Vite transpiles TS on the fly)
      { find: "@qontinui/ui-bridge-auto", replacement: path.resolve(__dirname, "../ui-bridge-auto/src/index.ts") },
      // workflow-ui subpaths resolve to source (Vite transpiles TSX on the fly)
      { find: "@qontinui/workflow-ui/state-machine", replacement: path.resolve(__dirname, "../qontinui-workflow-ui/src/components/state-machine/index.ts") },
      { find: "@qontinui/workflow-ui/chat", replacement: path.resolve(__dirname, "../qontinui-workflow-ui/src/components/chat/index.ts") },
      { find: /^@qontinui\/workflow-ui\/(.+)$/, replacement: path.resolve(__dirname, "../qontinui-workflow-ui/src/$1/index.ts") },
    ],
    // Prevent duplicate React/library instances from symlinked packages
    dedupe: ["react", "react-dom", "@xyflow/react", "@xyflow/system"],
  },
});
