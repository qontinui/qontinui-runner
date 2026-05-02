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
