#!/usr/bin/env node
// @ts-check
/**
 * Page-spec -> source-path map producer (Phase 1b of plan
 * 2026-06-04-page-spec-path-map-producer).
 *
 * Walks every `specs/pages/<page_id>/spec.uibridge.json`, resolves the source
 * paths that render each page, and POSTs the map to coord's page-spec ingest
 * route. Best-effort: a non-2xx response (e.g. the route isn't deployed yet)
 * is logged as a warning and the process exits 0 -- this producer must NEVER
 * gate CI. Pass `--strict` to make a non-2xx exit 1 (local verification only).
 *
 * Pure Node >= 18 stdlib. NO npm dependencies (the CI step runs it without
 * `pnpm install`). All file reads are UTF-8 (some spec files are not valid
 * cp1252).
 *
 * Resolution per page (see plan Phase 1):
 *   a. `src/components/<metadata.component>/` exists -> that directory prefix.
 *   b. else parse `src/components/app/TabContent.tsx`: find the
 *      `case "<page_id>":` arm, identify the rendered component(s), map them to
 *      their import paths (static or lazy), resolve each to a repo-relative
 *      existing path (directory -> prefix with trailing slash, file -> file).
 *   c. else SKIP -> honest gap (stays `unknown` coord-side). Never guess.
 *
 * Flags:
 *   --dry-run   print the body + per-page resolution table + gaps, no POST
 *   --strict    non-2xx response -> exit 1 (default: warn + exit 0)
 */

import { readFileSync, readdirSync, existsSync, statSync } from "node:fs";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";

const __filename = fileURLToPath(import.meta.url);
// scripts/page_spec_paths/ -> repo root is two levels up.
const REPO_ROOT = path.resolve(path.dirname(__filename), "..", "..");

const ARGS = new Set(process.argv.slice(2));
const DRY_RUN = ARGS.has("--dry-run");
const STRICT = ARGS.has("--strict");

const REPO_NAME = "qontinui-runner";
const APP_ID = "qontinui-runner";
const COORD_URL = ((process.env.COORD_HTTP_URL || "").trim() || "https://coord.qontinui.io").replace(
  /\/+$/,
  "",
);
const INGEST_PATH = "/coord/page-specs/paths";

const SPECS_DIR = path.join(REPO_ROOT, "specs", "pages");
const COMPONENTS_DIR = path.join(REPO_ROOT, "src", "components");
const TAB_CONTENT_PATH = path.join(REPO_ROOT, "src", "components", "app", "TabContent.tsx");

/** Convert an absolute path under the repo into a repo-relative, forward-slash path. */
function toRepoRel(absPath) {
  return path.relative(REPO_ROOT, absPath).split(path.sep).join("/");
}

/** Read a file as UTF-8 (specs may not be valid cp1252). */
function readUtf8(p) {
  return readFileSync(p, "utf8");
}

/** Full 40-hex HEAD sha. */
function headSha() {
  try {
    return execFileSync("git", ["rev-parse", "HEAD"], { cwd: REPO_ROOT })
      .toString()
      .trim();
  } catch (err) {
    console.warn(`[warn] could not resolve HEAD sha via git: ${err && err.message}`);
    return "0000000000000000000000000000000000000000";
  }
}

// --------------------------------------------------------------------------
// TabContent.tsx parsing
// --------------------------------------------------------------------------

/**
 * Build a map of imported-symbol -> import specifier string from TabContent.tsx.
 * Handles:
 *   import { A, B as C } from "spec";
 *   import D from "spec";
 *   const X = lazy(() => import("spec"));
 *   const X = lazy(() => import("spec").then((m) => ({ default: m.Y })));
 */
function buildImportMap(src) {
  /** @type {Map<string, string>} symbol -> import specifier */
  const map = new Map();

  // Static imports: `import ... from "specifier";`
  const importRe = /import\s+([^;]+?)\s+from\s+["']([^"']+)["']/g;
  let m;
  while ((m = importRe.exec(src)) !== null) {
    const clause = m[1].trim();
    const spec = m[2];
    // Default import (no braces) e.g. `import Foo` or `Foo, { ... }`
    const defaultMatch = clause.match(/^([A-Za-z_$][\w$]*)\s*(,|$)/);
    if (defaultMatch && !clause.startsWith("{") && !clause.startsWith("*")) {
      map.set(defaultMatch[1], spec);
    }
    // Named imports inside braces
    const brace = clause.match(/\{([^}]*)\}/);
    if (brace) {
      for (const raw of brace[1].split(",")) {
        const name = raw.trim();
        if (!name) continue;
        // `Orig as Alias` -> the local name is Alias
        const asMatch = name.match(/^[A-Za-z_$][\w$]*\s+as\s+([A-Za-z_$][\w$]*)$/);
        const local = asMatch ? asMatch[1] : name;
        map.set(local, spec);
      }
    }
  }

  // Lazy imports: `const X = lazy(() => import("specifier")...)`
  const lazyRe = /const\s+([A-Za-z_$][\w$]*)\s*=\s*lazy\(\s*\(\)\s*=>\s*[\s\S]*?import\(\s*["']([^"']+)["']\s*\)/g;
  while ((m = lazyRe.exec(src)) !== null) {
    map.set(m[1], m[2]);
  }

  return map;
}

/**
 * Resolve a TS import specifier (with @/ alias or relative path) to an existing
 * repo-relative path. Returns either a directory prefix (trailing slash) when
 * the specifier points at a directory/index file, or a single file path.
 * Returns null if nothing on disk matches.
 */
function resolveImportSpecifier(spec) {
  let absBase;
  if (spec.startsWith("@/")) {
    absBase = path.join(REPO_ROOT, "src", spec.slice(2));
  } else if (spec.startsWith(".")) {
    absBase = path.resolve(path.dirname(TAB_CONTENT_PATH), spec);
  } else {
    // bare package import -> not a repo path
    return null;
  }

  // Directory? -> emit the directory prefix.
  if (existsSync(absBase) && statSync(absBase).isDirectory()) {
    return { repoPath: toRepoRel(absBase) + "/", isDir: true };
  }

  // File with an explicit/implicit extension.
  const exts = ["", ".tsx", ".ts", ".jsx", ".js"];
  for (const ext of exts) {
    const candidate = absBase + ext;
    if (existsSync(candidate) && statSync(candidate).isFile()) {
      return { repoPath: toRepoRel(candidate), isDir: false };
    }
  }

  // index file inside a directory that itself doesn't exist literally
  for (const idx of ["index.tsx", "index.ts", "index.jsx", "index.js"]) {
    const candidate = path.join(absBase, idx);
    if (existsSync(candidate) && statSync(candidate).isFile()) {
      return { repoPath: toRepoRel(absBase) + "/", isDir: true };
    }
  }

  return null;
}

// JSX wrapper/chrome components that never represent the page's own source.
const WRAPPER_TAGS = new Set([
  "div",
  "span",
  "Suspense",
  "LazyFallback",
  "PageRegistration",
  "ErrorBoundary",
  "RunSelectionProvider",
  "RunPageLayout",
  "Fragment",
]);

/**
 * Parse the TabContent.tsx switch and return a Map of page_id -> array of
 * rendered component names (in source order, de-duplicated). Handles
 * fall-through case labels (consecutive `case` with no intervening body share
 * the body that follows the last label in the run).
 */
function parseTabContentCases(src) {
  // Locate each `case "id":` label with its offset.
  const caseRe = /case\s+["']([^"']+)["']\s*:/g;
  /** @type {{ id: string, start: number, end: number }[]} */
  const labels = [];
  let m;
  while ((m = caseRe.exec(src)) !== null) {
    labels.push({ id: m[1], start: m.index, end: caseRe.lastIndex });
  }
  // Also capture `default:` and end-of-switch as terminators.
  const defaultIdx = (() => {
    const dm = /\n\s*default\s*:/.exec(src);
    return dm ? dm.index : src.length;
  })();

  /** @type {Map<string, string[]>} */
  const result = new Map();

  for (let i = 0; i < labels.length; i++) {
    const label = labels[i];
    // Body extends from this label's end to the next label's start (or default/end).
    const next = labels[i + 1];
    const bodyEnd = next ? next.start : defaultIdx;
    const between = src.slice(label.end, bodyEnd);

    // Fall-through: if there is no real body before the next case label
    // (i.e. the text between is only whitespace), this case shares the body
    // of a later label. We detect that by checking whether `between` contains
    // a `return` statement.
    const hasBody = /\breturn\b/.test(between);

    let bodySrc;
    if (hasBody) {
      bodySrc = between;
    } else {
      // Walk forward to the first subsequent label that has a body.
      let j = i + 1;
      while (j < labels.length) {
        const e = labels[j + 1] ? labels[j + 1].start : defaultIdx;
        const chunk = src.slice(labels[j].end, e);
        if (/\breturn\b/.test(chunk)) {
          bodySrc = chunk;
          break;
        }
        j++;
      }
      if (bodySrc === undefined) bodySrc = between;
    }

    // Extract capitalized JSX component tags from the body.
    const tagRe = /<([A-Z][A-Za-z0-9_]*)\b/g;
    const seen = new Set();
    const comps = [];
    let t;
    while ((t = tagRe.exec(bodySrc)) !== null) {
      const tag = t[1];
      if (WRAPPER_TAGS.has(tag)) continue;
      if (seen.has(tag)) continue;
      seen.add(tag);
      comps.push(tag);
    }
    result.set(label.id, comps);
  }

  return result;
}

// --------------------------------------------------------------------------
// Resolution
// --------------------------------------------------------------------------

function listPageSpecs() {
  if (!existsSync(SPECS_DIR)) return [];
  return readdirSync(SPECS_DIR)
    .filter((name) => {
      const specPath = path.join(SPECS_DIR, name, "spec.uibridge.json");
      return existsSync(specPath) && statSync(specPath).isFile();
    })
    .sort();
}

function readComponentSlug(pageId) {
  const specPath = path.join(SPECS_DIR, pageId, "spec.uibridge.json");
  try {
    const doc = JSON.parse(readUtf8(specPath));
    const md = doc && doc.metadata;
    return md && typeof md.component === "string" ? md.component : null;
  } catch (err) {
    console.warn(`[warn] failed to parse ${toRepoRel(specPath)}: ${err && err.message}`);
    return null;
  }
}

function main() {
  const tabSrc = existsSync(TAB_CONTENT_PATH) ? readUtf8(TAB_CONTENT_PATH) : "";
  const importMap = buildImportMap(tabSrc);
  const caseMap = parseTabContentCases(tabSrc);

  const pageIds = listPageSpecs();

  /** @type {{ page_id: string, covered_paths: string[] }[]} */
  const pages = [];
  /** @type {{ page_id: string, component: string|null, via: string, paths: string[] }[]} */
  const table = [];
  /** @type {string[]} */
  const gaps = [];

  for (const pageId of pageIds) {
    const component = readComponentSlug(pageId);
    let coveredPaths = [];
    let via = "none";

    // (a) direct component directory.
    if (component) {
      const compDir = path.join(COMPONENTS_DIR, component);
      if (existsSync(compDir) && statSync(compDir).isDirectory()) {
        coveredPaths = [toRepoRel(compDir) + "/"];
        via = "component-dir";
      }
    }

    // (b) TabContent fallback.
    if (coveredPaths.length === 0) {
      const comps = caseMap.get(pageId) || [];
      const resolved = new Set();
      for (const compName of comps) {
        const spec = importMap.get(compName);
        if (!spec) continue;
        const r = resolveImportSpecifier(spec);
        if (r) resolved.add(r.repoPath);
      }
      if (resolved.size > 0) {
        coveredPaths = [...resolved].sort();
        via = "tabcontent";
      }
    }

    if (coveredPaths.length === 0) {
      gaps.push(pageId);
      table.push({ page_id: pageId, component, via: "GAP", paths: [] });
      continue;
    }

    pages.push({ page_id: pageId, covered_paths: coveredPaths });
    table.push({ page_id: pageId, component, via, paths: coveredPaths });
  }

  const body = {
    repo: REPO_NAME,
    head_sha: headSha(),
    app_id: APP_ID,
    pages,
  };

  // ---- reporting ----
  if (DRY_RUN) {
    console.log("=== PER-PAGE RESOLUTION ===");
    for (const row of table) {
      console.log(
        `${row.page_id}\t[${row.via}]\tcomponent=${row.component ?? "-"}\t-> ${
          row.paths.length ? row.paths.join(", ") : "(gap)"
        }`,
      );
    }
    console.log("\n=== POST BODY ===");
    console.log(JSON.stringify(body, null, 2));
  }

  console.log("\n=== SUMMARY ===");
  console.log(`pages total : ${pageIds.length}`);
  console.log(`mapped      : ${pages.length}`);
  console.log(`gaps        : ${gaps.length}`);
  if (gaps.length) console.log(`gap page_ids: ${gaps.join(", ")}`);
  console.log(`coord url   : ${COORD_URL}${INGEST_PATH}`);

  if (DRY_RUN) {
    console.log("\n[dry-run] no POST performed.");
    return;
  }

  postBody(body);
}

async function postBody(body) {
  const url = `${COORD_URL}${INGEST_PATH}`;
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    const text = await res.text();
    console.log(`\n[post] ${res.status} ${res.statusText} -> ${url}`);
    console.log(`[post] response body: ${text.slice(0, 2000)}`);
    if (res.status < 200 || res.status >= 300) {
      if (STRICT) {
        console.error(`[strict] non-2xx response (${res.status}); exiting 1.`);
        process.exit(1);
      }
      console.warn(
        `[warn] non-2xx response (${res.status}); best-effort producer, exiting 0. ` +
          `This is expected until the coord ingest route + web migration are deployed.`,
      );
    }
  } catch (err) {
    const msg = err && err.message ? err.message : String(err);
    if (STRICT) {
      console.error(`[strict] POST failed: ${msg}; exiting 1.`);
      process.exit(1);
    }
    console.warn(`[warn] POST failed: ${msg}; best-effort producer, exiting 0.`);
  }
}

main();
