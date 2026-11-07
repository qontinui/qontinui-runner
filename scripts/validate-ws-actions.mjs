#!/usr/bin/env node
// Cross-language action-name validator + action-vocabulary doc generator.
//
// The runner's `dispatch_app_request(state, "<action>", ...)` call sites in
// sdk_client.rs — and its siblings `dispatch_app_request_typed(state,
// "<action>", ...)`, `dispatch_app_request_by_id(state, app_id, "<action>",
// ...)` and legacy `try_ws_dispatch(state, "<action>", payload)` — send
// `<action>` as the WS frame's action field. The wrapper SDK's
// HandlerRegistry dispatches each frame to the handler keyed by that string
// verbatim — a typo or rename on either side fails silently with NO_HANDLER
// at runtime.
//
// Default mode: extract every action name from both files and report any
// runner-side action that has no matching handler in the wrapper SDK's
// relay-handlers.ts vocabulary. Exits non-zero on drift.
//
// --emit-doc <path>: build a markdown action-vocabulary table from sdk_client.rs
//   (every handler with `/// METHOD /path — desc\n[pub[(crate)] ]async fn
//    handle_X(...)` and a dispatch site) and write it to <path>. <path>
//   resolves against the WORKSPACE root (the directory holding the
//   qontinui-runner and ui-bridge checkouts), so the ui-bridge doc is
//   `ui-bridge/docs-site/docs/wrappers/action-vocabulary.md` — see the
//   `vocab:gen` / `vocab:check` npm scripts.
//
// --check-doc <path>: compare what --emit-doc would write to the file at
//   <path>; exit non-zero if they differ. Same shape as `npm run graphql:check`.

import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const repoRoot = resolve(__dirname, "..", "..");

const sdkClientPath = resolve(
  repoRoot,
  "qontinui-runner",
  "src-tauri",
  "src",
  "mcp",
  "sdk_client.rs",
);
const relayHandlersPath = resolve(
  repoRoot,
  "ui-bridge",
  "packages",
  "ui-bridge",
  "src",
  "server",
  "relay-handlers.ts",
);

function readFile(path) {
  try {
    return readFileSync(path, "utf8");
  } catch (err) {
    console.error(`fatal: cannot read ${path}: ${err.message}`);
    process.exit(2);
  }
}

// Match every dispatch entry point that carries a literal action name:
//   dispatch_app_request(&state, "<name>", ...
//   dispatch_app_request_typed(&state, "<name>", ...
//   dispatch_app_request_by_id(&state, <app_id>, "<name>", ...
//   try_ws_dispatch(&state, "<name>", ...
// `\s*` between tokens crosses newlines, so multi-line call shapes like
//   dispatch_app_request(
//       &state,
//       "<name>",
//       ...
//   )
// are matched too. The action string is a "double-quoted" literal: the second
// argument, or the third for the `_by_id` form (whose second is the app id).
//
// DISPATCH_SITE is shared with `extractRouteTable` so the two can never again
// disagree about which entry points count. Its one capture group is the
// action name.
const DISPATCH_SITE =
  String.raw`\b(?:(?:dispatch_app_request(?:_typed)?|try_ws_dispatch)\(\s*&state,\s*` +
  String.raw`|dispatch_app_request_by_id\(\s*&state,\s*&?[A-Za-z_][A-Za-z0-9_.]*,\s*)` +
  String.raw`"([^"]+)"`;

export function extractRunnerActions(src) {
  const out = new Set();
  for (const m of src.matchAll(new RegExp(DISPATCH_SITE, "g"))) out.add(m[1]);
  return out;
}

// Match the wrapper-SDK relay vocabulary. Three call shapes coexist in
// relay-handlers.ts:
//   relayCommand('<name>'[, ...])
//   relay.queueCommand<...>('<name>'[, ...])
//   relayWithFallback('<name>'[, ...])
export function extractWrapperActions(src) {
  const patterns = [
    /\brelayCommand(?:<[^>]*>)?\(\s*['"]([A-Za-z0-9_]+)['"]/g,
    /\.queueCommand(?:<[^>]*>)?\(\s*['"]([A-Za-z0-9_]+)['"]/g,
    /\brelayWithFallback(?:<[^>]*>)?\(\s*['"]([A-Za-z0-9_]+)['"]/g,
  ];
  const out = new Set();
  for (const re of patterns) {
    for (const m of src.matchAll(re)) out.add(m[1]);
  }
  return out;
}

// Build an (action, method, path, handler, description) table from
// sdk_client.rs. Each retrofitted handler has the shape
//
//   /// METHOD /path — description
//   /// (optional further doc lines)
//   [pub |pub(crate) ]async fn handle_X(...) -> ... {
//       ...
//       dispatch_app_request(&state, "<action>", ...)
//   }
//
// We pair each `/// METHOD /path — desc` doc line and the `async fn handle_X`
// signature it documents with the FIRST dispatch site inside the handler body
// — any of the entry points `extractRunnerActions` matches — refusing to
// cross into the next `async fn`. Handlers without a dispatch site (e.g.
// IPC-only handlers) are skipped intentionally — they don't belong in the
// action vocabulary table.
export function extractRouteTable(src) {
  // `(?:\/\/\/[^\n]*\n)*` admits further `///` lines between the route line
  // and the signature; `VIS` admits `pub ` / `pub(crate) ` in front of
  // `async fn`. The negative lookahead `(?!\n${VIS}async fn )` prevents the
  // lazy `[\s\S]*?` from running past the handler's closing brace into the
  // next handler.
  //
  // Not admitted (none occur in sdk_client.rs today; a handler using one is
  // silently left out of the table): `pub(in path)` visibility, and a `#[...]`
  // attribute between the doc comment and the signature.
  const VIS = String.raw`(?:pub(?:\([a-z]+\))? )?`;
  const re = new RegExp(
    String.raw`\/\/\/\s+(GET|POST|PUT|DELETE|PATCH)\s+(\S+)(?:\s+—\s+([^\n]+))?\n` +
      String.raw`(?:\/\/\/[^\n]*\n)*` +
      String.raw`${VIS}async fn (handle_[a-z0-9_]+)\b` +
      String.raw`((?:(?!\n${VIS}async fn )[\s\S])*?)` +
      DISPATCH_SITE,
    "g",
  );
  const rows = [];
  for (const m of src.matchAll(re)) {
    rows.push({
      method: m[1],
      path: m[2],
      desc: (m[3] ?? "").trim(),
      handler: m[4],
      action: m[6],
    });
  }
  // Sort by action, then path, for stable output.
  rows.sort((a, b) => a.action.localeCompare(b.action) || a.path.localeCompare(b.path));
  return rows;
}

function renderRouteTable(rows) {
  const lines = [];
  lines.push("| Action | HTTP route | Handler |");
  lines.push("| --- | --- | --- |");
  for (const r of rows) {
    const desc = r.desc ? ` — ${r.desc}` : "";
    lines.push(`| \`${r.action}\` | \`${r.method} ${r.path}\`${desc} | \`${r.handler}\` |`);
  }
  return lines.join("\n") + "\n";
}

// `--emit-doc <path>` and `--check-doc <path>` are doc-generation modes that
// produce a deterministic markdown table extracted from sdk_client.rs. The
// table is wrapped in HTML comment markers so action-vocabulary.md can have
// hand-written prose around it, and the doc-check pattern (similar to
// `npm run graphql:check`) catches drift in CI.
const BEGIN = "<!-- BEGIN auto-generated by scripts/validate-ws-actions.mjs -->";
const END = "<!-- END auto-generated -->";

export function buildDocSection(rows) {
  // `<!-- prettier-ignore -->` keeps the prettier hook from rewriting the
  // generated table with column padding — otherwise --check-doc would always
  // fail post-commit because the on-disk shape (padded) wouldn't match what
  // --emit-doc writes (unpadded).
  //
  // The regenerate command names the path the way the script resolves it —
  // against the workspace root, so `ui-bridge/...` — and is run from the
  // qontinui-runner checkout (it is that repo's `vocab:gen` npm script).
  const header = `${BEGIN}\n\n_${rows.length} routes; regenerate from a qontinui-runner checkout (with ui-bridge beside it) via \`npm run vocab:gen\`, i.e. \`node scripts/validate-ws-actions.mjs --emit-doc ui-bridge/docs-site/docs/wrappers/action-vocabulary.md\`._\n\n<!-- prettier-ignore -->\n`;
  return `${header}${renderRouteTable(rows)}\n${END}`;
}

export function replaceAutoSection(existing, replacement) {
  const startIdx = existing.indexOf(BEGIN);
  const endIdx = existing.indexOf(END);
  if (startIdx === -1 || endIdx === -1 || endIdx < startIdx) {
    // No existing markers — append at the end of the file with a blank line.
    return existing.replace(/\s*$/, "\n\n") + replacement + "\n";
  }
  return existing.slice(0, startIdx) + replacement + existing.slice(endIdx + END.length);
}

function main() {
  const argEmit = process.argv.indexOf("--emit-doc");
  const argCheck = process.argv.indexOf("--check-doc");
  if (argEmit !== -1 || argCheck !== -1) {
    const idx = argEmit !== -1 ? argEmit : argCheck;
    const docPath = process.argv[idx + 1];
    if (!docPath) {
      console.error(`fatal: ${process.argv[idx]} requires a path argument (target markdown file)`);
      process.exit(2);
    }
    const rows = extractRouteTable(readFile(sdkClientPath));
    if (rows.length === 0) {
      console.error("fatal: no handler/dispatch pairs found in sdk_client.rs");
      process.exit(2);
    }
    const replacement = buildDocSection(rows);
    const targetPath = resolve(repoRoot, docPath);
    let existing;
    try {
      existing = readFileSync(targetPath, "utf8");
    } catch (err) {
      console.error(`fatal: cannot read ${targetPath}: ${err.message}`);
      process.exit(2);
    }
    const next = replaceAutoSection(existing, replacement);
    if (argEmit !== -1) {
      if (next === existing) {
        console.log(`ok: ${docPath} already up to date (${rows.length} routes)`);
      } else {
        writeFileSync(targetPath, next);
        console.log(`wrote: ${docPath} (${rows.length} routes)`);
      }
      process.exit(0);
    }
    // --check-doc
    if (next !== existing) {
      console.error(
        `ERROR: ${docPath} is stale — re-run \`npm run validate:ws-actions -- --emit-doc ${docPath}\` and commit the result`,
      );
      process.exit(1);
    }
    console.log(`ok: ${docPath} matches generated content (${rows.length} routes)`);
    process.exit(0);
  }

  const runnerActions = extractRunnerActions(readFile(sdkClientPath));
  const wrapperActions = extractWrapperActions(readFile(relayHandlersPath));

  if (runnerActions.size === 0) {
    console.error(
      "fatal: no dispatch_app_request or try_ws_dispatch call sites found in sdk_client.rs",
    );
    process.exit(2);
  }
  if (wrapperActions.size === 0) {
    console.error("fatal: no relay-handler action names found in relay-handlers.ts");
    process.exit(2);
  }

  const missing = [...runnerActions].filter((a) => !wrapperActions.has(a)).sort();
  const orphans = [...wrapperActions].filter((a) => !runnerActions.has(a)).sort();

  console.log(
    `runner sites=${runnerActions.size}, wrapper actions=${wrapperActions.size}, ` +
      `missing=${missing.length}, orphans=${orphans.length}`,
  );

  if (missing.length > 0) {
    console.error("\nERROR: runner dispatches actions the wrapper SDK has no handler for:");
    for (const a of missing) console.error(`  - ${a}`);
    console.error("\nA WS-transport wrapper will fail with NO_HANDLER for these actions.");
    console.error(
      "Either add the handler in ui-bridge/packages/ui-bridge/src/server/relay-handlers.ts,",
    );
    console.error(
      "or change the runner-side action name in sdk_client.rs to match an existing handler.",
    );
    process.exit(1);
  }

  // Orphans are informational only — a wrapper-SDK action with no runner caller
  // just means the runner doesn't proxy that surface yet. Not an error.
  if (process.argv.includes("--verbose") && orphans.length > 0) {
    console.log("\nwrapper actions with no runner caller (informational):");
    for (const a of orphans) console.log(`  - ${a}`);
  }

  console.log("ok: every runner action has a matching wrapper handler");
}

// Run only when invoked as a script, so scripts/__tests__ can import the
// extractors without the CLI reading the sibling checkouts or exiting.
if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  main();
}
