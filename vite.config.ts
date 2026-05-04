import path from "path";
import { execSync } from "child_process";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { version } from "./package.json";

// Compute the runner build-id at Vite-build time. Format
// `<git-sha-short>-<unix-ms>`. Vite is the *single source of truth* for the
// build-id: the value is baked into the embedded index.html as
// `<meta name="build-id">` AND written to `dist/build-id.txt`, which
// `build.rs` reads back when stamping `RUNNER_BUILD_ID` into the binary.
// This guarantees the meta tag and the cargo-baked env match for any
// freshly-built binary, so `useBuildIdWatcher` only fires on a real
// mid-session binary swap (not on every cold spawn — Vite and cargo used
// to capture independent timestamps tens of seconds apart, which produced
// false positives on every fresh build).
function computeBuildId(): string {
  let sha = "unknown";
  try {
    sha = execSync("git rev-parse --short HEAD", { stdio: ["ignore", "pipe", "ignore"] })
      .toString()
      .trim();
  } catch {
    // git missing or not a repo — fall through to "unknown"
  }
  return `${sha || "unknown"}-${Date.now()}`;
}
const RUNNER_BUILD_ID = computeBuildId();

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    // Inject `<meta name="build-id" content="...">` into the served HTML so
    // `useBuildIdWatcher` (consumed by BuildRefreshBanner) has a stable
    // initial value to compare against `invoke('get_build_id')`. Runs in
    // `transformIndexHtml` (early) so the strip-module-attrs and
    // extract-css-from-bundle plugins later in this list don't have to know
    // about it.
    {
      name: "inject-build-id-meta",
      enforce: "pre" as const,
      transformIndexHtml(html: string) {
        const escaped = RUNNER_BUILD_ID.replace(/"/g, "&quot;");
        const tag = `<meta name="build-id" content="${escaped}">`;
        if (html.includes('name="build-id"')) {
          return html.replace(
            /<meta\s+name="build-id"[^>]*>/,
            tag,
          );
        }
        return html.replace("</head>", `    ${tag}\n  </head>`);
      },
      // Emit dist/build-id.txt so `build.rs` can read the same value Vite
      // baked into the meta tag. Without this, Vite and cargo would each
      // capture an independent `Date.now()` and the resulting RUNNER_BUILD_ID
      // would never match the meta tag — the banner would fire on every
      // fresh spawn rather than only on real mid-session binary swaps.
      generateBundle() {
        this.emitFile({
          type: "asset",
          fileName: "build-id.txt",
          source: RUNNER_BUILD_ID,
        });
      },
    },
    // Strip module-related attributes from built HTML. The IIFE bundle
    // doesn't use ES module syntax, so type="module" and crossorigin are
    // unnecessary. Removing type="module" is critical: it makes the browser
    // use the classic script loader, bypassing WebView2's broken module
    // fetcher on cold custom-protocol profiles.
    // IMPORTANT: We must add "defer" when stripping type="module", because
    // type="module" implicitly defers execution until after DOM parsing.
    // Without defer, the IIFE runs in <head> before <div id="root"> exists,
    // so React mounts to null and the app shows "Loading..." forever.
    {
      name: "strip-module-attrs",
      enforce: "post" as const,
      transformIndexHtml(html: string) {
        return html
          .replace(/ crossorigin/g, "")
          .replace(/ type="module"/g, " defer")
          .replace(/<link rel="modulepreload"[^>]*>/g, "");
      },
    },
    // Extract CSS from the JS bundle and inline it into index.html.
    //
    // Problem: The IIFE rollup format with @tailwindcss/vite causes Tailwind
    // CSS to be embedded in the JS bundle as runtime-injected styles. This
    // is unreliable — the injection can fail or arrive late, leaving the app
    // with no Tailwind utilities (display:block instead of display:flex).
    //
    // Solution: After the bundle is generated, scan the JS chunk for CSS
    // strings (they're in __vite_style__ or injectStyle calls), extract them,
    // and write them as a <style> tag in index.html. This guarantees styles
    // are available before any JS executes.
    {
      name: "extract-css-from-bundle",
      enforce: "post" as const,
      generateBundle(_options, bundle) {
        // First check if there are any CSS asset files (normal extraction path)
        let cssContent = "";
        const cssAssetNames: string[] = [];
        for (const [fileName, chunk] of Object.entries(bundle)) {
          if (fileName.endsWith(".css") && chunk.type === "asset") {
            cssContent +=
              typeof chunk.source === "string"
                ? chunk.source
                : new TextDecoder().decode(chunk.source as Uint8Array);
            cssAssetNames.push(fileName);
          }
        }

        // If no CSS assets, try extracting from the JS bundle
        if (!cssContent) {
          for (const chunk of Object.values(bundle)) {
            if (chunk.type === "chunk" && chunk.code) {
              // Look for large CSS strings in __vite_style__ or injectStyle calls
              // Pattern: __vite_style__("...css...") or injectStyle("...css...")
              const re = /(?:__vite_style__|injectStyle(?:s)?)\s*\(\s*["'`]([\s\S]*?)["'`]\s*\)/g;
              let match;
              while ((match = re.exec(chunk.code)) !== null) {
                if (match[1].length > 500) {
                  const css = match[1]
                    .replace(/\\n/g, "\n")
                    .replace(/\\t/g, "\t")
                    .replace(/\\"/g, '"')
                    .replace(/\\\\/g, "\\");
                  cssContent += css + "\n";
                }
              }
            }
          }
        }

        if (!cssContent) {
          console.log("[extract-css] No CSS found in bundle — Tailwind may not be generating output");
          return;
        }

        console.log(`[extract-css] Extracted ${(cssContent.length / 1024).toFixed(0)}KB of CSS`);

        // Inject into index.html
        for (const [fileName, chunk] of Object.entries(bundle)) {
          if (fileName === "index.html" && chunk.type === "asset") {
            let html =
              typeof chunk.source === "string"
                ? chunk.source
                : new TextDecoder().decode(chunk.source as Uint8Array);

            // Remove any <link> tags pointing to CSS assets we're inlining
            for (const cssName of cssAssetNames) {
              const pattern = new RegExp(
                `<link[^>]*href="[^"]*${cssName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}[^"]*"[^>]*>`,
                "g"
              );
              html = html.replace(pattern, "");
              delete bundle[cssName];
            }

            html = html.replace("</head>", `<style>${cssContent}</style>\n</head>`);
            chunk.source = html;
            break;
          }
        }
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
    // Tauri uses Chromium on Windows and WebKit on macOS and Linux. When
    // `npm run build` is invoked outside `tauri build` (e.g. by an agent
    // pre-staging dist/ before a supervisor spawn), TAURI_PLATFORM is unset
    // — fall back to the host's process.platform so we match the actual
    // webview engine instead of guessing.
    target: (() => {
      const tauriPlatform = process.env.TAURI_PLATFORM;
      if (tauriPlatform === "windows") return "chrome105";
      // macOS / Linux: WebKit on Tauri 2.5+ supports es2022. Bundled deps
      // (e.g. @qontinui/ui-bridge transitively) use newer ES syntax (async
      // destructuring with rename) that esbuild can't transpile down to
      // safari13/14 — those targets hard-error with 7000+ destructuring
      // errors during the build.
      if (tauriPlatform === "darwin") return "es2022";
      return process.platform === "darwin" ? "es2022" : "chrome105";
    })(),
    // don't minify for debug builds
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    // produce sourcemaps for debug builds
    sourcemap: !!process.env.TAURI_DEBUG,
    // Workaround for WebView2 cold-profile module loading failure:
    // WebView2's ES module loader fails to fetch modules through Tauri's
    // custom-protocol on fresh profiles. Using IIFE format with inlined
    // dynamic imports produces a single classic <script> that bypasses the
    // module loader entirely. All assets are embedded in the binary anyway,
    // so code-splitting provides no network benefit.
    // Force CSS to be emitted as a separate asset (not absorbed into the JS
    // IIFE bundle). The extract-css-from-bundle plugin then inlines it into
    // index.html as a <style> tag for reliable synchronous loading.
    cssCodeSplit: false,
    rollupOptions: {
      output: {
        format: "iife" as const,
        inlineDynamicImports: true,
      },
    },
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
      // @qontinui/* TS packages are now consumed from npm (^x.y.z ranges
      // in package.json). Vite resolves them via node_modules + package
      // exports maps; no per-package alias is needed. This block previously
      // hard-coded sibling-relative paths back when the deps were file:/link:.
    ],
    // Prevent duplicate React/library instances from symlinked packages
    dedupe: ["react", "react-dom", "@xyflow/react", "@xyflow/system"],
  },
});
