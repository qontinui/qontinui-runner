import js from "@eslint/js";
import typescript from "@typescript-eslint/eslint-plugin";
import typescriptParser from "@typescript-eslint/parser";
import react from "eslint-plugin-react";
import reactHooks from "eslint-plugin-react-hooks";
import uiBridgePlugin from "@qontinui/ui-bridge-eslint-plugin";
import globals from "globals";

export default [
  js.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      parser: typescriptParser,
      parserOptions: {
        ecmaVersion: 2022,
        sourceType: "module",
        ecmaFeatures: {
          jsx: true,
        },
      },
    },
    plugins: {
      "@typescript-eslint": typescript,
      react: react,
      "react-hooks": reactHooks,
      "@qontinui/ui-bridge": uiBridgePlugin,
    },
    settings: {
      react: {
        version: "detect",
      },
    },
    rules: {
      ...typescript.configs.recommended.rules,
      ...reactHooks.configs.recommended.rules,
      "no-useless-assignment": "error",
      "preserve-caught-error": "error",
      "react/react-in-jsx-scope": "off",
      "react/prop-types": "off",
      "@typescript-eslint/no-explicit-any": "warn",
      "@typescript-eslint/explicit-module-boundary-types": "off",
      "no-undef": "off", // TypeScript handles this
      "no-unused-vars": "off", // Use TypeScript's version
      "@typescript-eslint/no-unused-vars": [
        "warn",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
      // Prefer structured logger over console.* — use createLogger() from lib/logger
      "no-console": ["warn", { allow: ["warn", "error"] }],
      // react-hooks v7 introduced strict rules that flag many existing patterns.
      // Downgrade to warn so the codebase can be cleaned up incrementally rather
      // than blocking all commits until every file is updated.
      "react-hooks/set-state-in-effect": "warn",
      "react-hooks/refs": "warn",
      "react-hooks/immutability": "warn",
      "react-hooks/purity": "warn",
      "react-hooks/preserve-manual-memoization": "warn",
      // UI Bridge Section 3 / Phase B5d: plugin is wired but the rule is
      // intentionally OFF until the codebase is progressively annotated with
      // <State> wrappers. The runner's `lint` script uses `--max-warnings 0`,
      // so a 'warn' setting (1355 unannotated sites) breaks CI for unrelated
      // PRs. Flip this to 'warn' once the per-page annotation pass lands.
      "@qontinui/ui-bridge/require-state-annotation": "off",
      // Plan `2026-09-04-effect-calculus-joins-the-component-action-registry`,
      // Phase 3 — the ratchet, shipped ON.
      //
      // Every component action registered through `useUIComponent({ actions })`
      // must declare an `effect` (`read` | `write` | `destructive`). An absent
      // effect is UNCLASSIFIED, never `read`: the serializer forwards the field
      // undefaulted on purpose and no verb in the SDK's consumer-side
      // `STANDARD_ACTION_EFFECTS` map can yield `destructive`, so an
      // unannotated action is indistinguishable — to an agent deciding whether
      // it is safe to invoke — from a harmless one.
      //
      // `error`, NOT `warn`, and deliberately unlike the line above. Phase 2
      // annotated all 64 registered actions FIRST, so this rule starts green;
      // it is a ratchet on a clean corpus rather than a promise to clean one
      // up later. `require-state-annotation` shipped "off until the codebase is
      // progressively annotated" and has stayed off ever since across 1355
      // sites — a capability nobody switched on is a capability the product
      // does not have [policy: `capability-ships-enabled`]. Note also that
      // `lint` does NOT pass `--max-warnings 0`, so a `warn` here would be
      // invisible to CI: `error` is the only severity that gates anything.
      //
      // The enumerated-coverage test
      // (`src/lib/ui-bridge/action-effect-coverage.test.ts`) and this rule are
      // complements, not duplicates: the test proves the corpus is annotated
      // today and cannot silently shrink, while the rule refuses the next
      // unannotated action at the moment it is written.
      //
      // `importAliases` lets the rule follow an action factory imported through
      // the tsconfig `@/*` path (see tsconfig.json `compilerOptions.paths`) —
      // resolution configuration, not a suppression list. An import it cannot
      // follow is REPORTED as unresolved, never assumed annotated.
      "@qontinui/ui-bridge/require-action-effect": ["error", { importAliases: { "@/": "src" } }],
    },
  },
  {
    // Configuration for browser extension files (service workers)
    files: ["extension/**/*.js"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "script",
      globals: {
        ...globals.browser,
        ...globals.webextensions,
        chrome: "readonly",
      },
    },
    rules: {
      "no-unused-vars": ["warn", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
    },
  },
  {
    ignores: [
      "node_modules/**",
      "dist/**",
      "src-tauri/**",
      "*.config.js",
      "python-bridge/**",
      "test_venv/**",
      "*.mjs",
      "*.js",
      "!src/**/*.js",
      "!extension/**/*.js",
      "src/generated/**",
    ],
  },
];
