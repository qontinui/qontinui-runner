import js from "@eslint/js";
import typescript from "@typescript-eslint/eslint-plugin";
import typescriptParser from "@typescript-eslint/parser";
import react from "eslint-plugin-react";
import reactHooks from "eslint-plugin-react-hooks";
import uiBridgePlugin from "@qontinui/ui-bridge-eslint-plugin";
import globals from "globals";

/**
 * Plan `2026-08-23-single-source-derived-facts`, item 12b(b) — the
 * rename-enforcer for the Terminal page's population names.
 *
 * The Terminal page counts three genuinely different populations that
 * routinely and legitimately differ in both directions: Claude transcript
 * sessions (machine-wide, incl. active-external ones with no tab here), open
 * PTY panes/tabs on this page, and layout zones (CSS grid cells). No type
 * distinguishes them — all three are `number` — so the only defence is the
 * name. The bare `sessionCount` has named at least three of them in this tree
 * (a status-strip gate whose input was silently re-pointed from tabs to Claude
 * sessions; a banner fed `tabs.length` that printed "N sessions open"), so it
 * is banned here as a DECLARED name — variable, destructured binding,
 * parameter, object / interface / class key, or JSX prop. Reading someone
 * else's field is not flagged; declaring one is.
 *
 * Deliberately narrow: this cannot catch a wrong population under a correct
 * name (that is domain semantics no AST carries). It only refuses the name
 * that makes the confusion free to write.
 *
 * Flat config REPLACES a rule's options across blocks rather than merging
 * them, so any later block that sets `no-restricted-syntax` for files under
 * `src/components/terminal/**` must spread `TERMINAL_POPULATION_NAME_SELECTORS`
 * into its own list, or this guard silently switches off there.
 */
const BANNED_POPULATION_NAMES = "/^(sessionCount|sessionNeedsInputCount)$/";
const POPULATION_NAME_MESSAGE =
  "Bare `sessionCount` / `sessionNeedsInputCount` names no population. The Terminal page " +
  "counts three that legitimately differ: Claude sessions (`claudeSessionCount`), open PTY " +
  "panes (`openPaneCount`) and layout zones (`zoneCount`). Name the one you mean " +
  "(plan 2026-08-23-single-source-derived-facts, item 12b).";
const TERMINAL_POPULATION_NAME_SELECTORS = [
  // const sessionCount = …
  `VariableDeclarator > Identifier.id[name=${BANNED_POPULATION_NAMES}]`,
  // const { sessionCount } = … / const { x: sessionCount } = …
  `ObjectPattern > Property > Identifier.value[name=${BANNED_POPULATION_NAMES}]`,
  // const [sessionCount] = …
  `ArrayPattern > Identifier[name=${BANNED_POPULATION_NAMES}]`,
  // function f(sessionCount) / (sessionCount = 0) => …
  `:function > Identifier.params[name=${BANNED_POPULATION_NAMES}]`,
  `:function > AssignmentPattern > Identifier.left[name=${BANNED_POPULATION_NAMES}]`,
  // type-level signatures: (sessionCount: number) => void
  `:matches(TSFunctionType, TSMethodSignature, TSDeclareFunction, TSCallSignatureDeclaration, TSConstructSignatureDeclaration) > Identifier.params[name=${BANNED_POPULATION_NAMES}]`,
  // { sessionCount: n } / interface { sessionCount: number } / class { sessionCount = 0 }
  // (ObjectExpression only: a destructuring key is a READ of someone else's
  // field, and its bound name is already covered by the ObjectPattern arm.)
  `ObjectExpression > Property > Identifier.key[name=${BANNED_POPULATION_NAMES}]`,
  `TSPropertySignature > Identifier.key[name=${BANNED_POPULATION_NAMES}]`,
  `PropertyDefinition > Identifier.key[name=${BANNED_POPULATION_NAMES}]`,
  // <SessionCountBanner sessionCount={…} />
  `JSXAttribute > JSXIdentifier.name[name=${BANNED_POPULATION_NAMES}]`,
].map((selector) => ({ selector, message: POPULATION_NAME_MESSAGE }));

/**
 * Plan `2026-09-09-catch-site-discard-is-repo-wide-and-the-deferral-was-never-measured`
 * — the catch-site discard guard.
 *
 * `x instanceof Error ? x.message : <anything>` throws the cause away on every
 * real Tauri failure: `invoke()` rejects with a plain STRING, so the `else`
 * arm is the one taken, and a bare constant (or `String(obj)` ->
 * `[object Object]`) is all the operator sees. `describeThrown(err, fallback)`
 * in `src/lib/utils.ts` is the one helper that keeps the cause.
 *
 * Deliberately NARROW: only the message-extracting shape is banned. The
 * value-preserving shapes stay legal — `err instanceof Error ? err : new
 * Error(String(err))` (normalisation) and `q.error instanceof Error ? q.error
 * : null`. A site whose constant is load-bearing (the raw cause would leak a
 * secret, or the swallow is deliberate) keeps it with an
 * `eslint-disable-next-line no-restricted-syntax -- <reason>` comment;
 * `--report-unused-disable-directives-severity error` fails a stale one.
 *
 * Flat config REPLACES `no-restricted-syntax` options across blocks (see the
 * population-name note above), so the terminal block spreads BOTH lists.
 */
const CATCH_SITE_DISCARD_MESSAGE =
  "Hand-rolled `x instanceof Error ? x.message : …` discards the cause of every non-Error " +
  'rejection (Tauri `invoke()` rejects with a STRING). Use `describeThrown(err, "<what failed>")` ' +
  "from `@/lib/utils` (plan 2026-09-09-catch-site-discard-is-repo-wide-and-the-deferral-was-never-measured).";
// Four spellings of one discard, measured on the whole src tree (plan
// 2026-10-08-catch-site-guard-escape-routes-and-diagnostic-logs-lost-the-raw-object).
// `/Error$/` covers subclasses (`instanceof TypeError ? e.message : …`); the
// ChainExpression arms cover `e?.message`; the negated arms cover
// `!(e instanceof Error) ? … : e.message`. Each REPLACES rather than
// accompanies the original plain-`Error` arm, so a ternary reports once.
// A ternary whose OTHER branch already calls describeThrown keeps the cause
// (`e instanceof ApiError ? e.message : describeThrown(e, "…")` — a deliberate
// subclass-specific text), so each arm excludes it with :not().
// Known residual: a qualified constructor (`instanceof ns.ApiError`) is not
// matched — its `right` is a MemberExpression.
const CATCH_SITE_DISCARD_SELECTORS = [
  'ConditionalExpression[test.type="BinaryExpression"][test.operator="instanceof"][test.right.name=/Error$/][consequent.type="MemberExpression"][consequent.property.name="message"]:not([alternate.callee.name="describeThrown"])',
  'ConditionalExpression[test.type="BinaryExpression"][test.operator="instanceof"][test.right.name=/Error$/][consequent.type="ChainExpression"][consequent.expression.type="MemberExpression"][consequent.expression.property.name="message"]:not([alternate.callee.name="describeThrown"])',
  'ConditionalExpression[test.type="UnaryExpression"][test.operator="!"][test.argument.type="BinaryExpression"][test.argument.operator="instanceof"][test.argument.right.name=/Error$/][alternate.type="MemberExpression"][alternate.property.name="message"]:not([consequent.callee.name="describeThrown"])',
  'ConditionalExpression[test.type="UnaryExpression"][test.operator="!"][test.argument.type="BinaryExpression"][test.argument.operator="instanceof"][test.argument.right.name=/Error$/][alternate.type="ChainExpression"][alternate.expression.property.name="message"]:not([consequent.callee.name="describeThrown"])',
].map((selector) => ({ selector, message: CATCH_SITE_DISCARD_MESSAGE }));

/**
 * The CAST spelling of the same discard, production files only:
 * `catch (err) { setError((err as Error).message) }`. A cast is not a check —
 * on a Tauri `invoke()` string rejection `.message` is `undefined`, so the user
 * sees a BLANK error. Scoped to a `catch` clause (where the value's type is
 * genuinely unknown) and to non-test files: a test's cast on an Error it
 * provoked itself is correct code, and a rule that fires on correct code gets
 * disabled. Known residual: `err as Error | undefined` (a union),
 * `<Error>err`, `(err as any).message` and `(err as {message: string}).message`
 * are not matched. That split is why the blocks below come in production/test pairs —
 * flat config REPLACES rule options, so each pair re-states its full list.
 */
const CATCH_SITE_CAST_MESSAGE =
  "`(err as Error).message` in a catch is a cast, not a check: a Tauri `invoke()` string " +
  'rejection yields `undefined`, a blank error. Use `describeThrown(err, "<what failed>")` ' +
  "from `@/lib/utils` (plan 2026-10-08-catch-site-guard-escape-routes-and-diagnostic-logs-lost-the-raw-object).";
const CATCH_SITE_CAST_SELECTORS = [
  'CatchClause MemberExpression[property.name="message"][object.type="TSAsExpression"][object.typeAnnotation.typeName.name=/Error$/]',
].map((selector) => ({ selector, message: CATCH_SITE_CAST_MESSAGE }));

const TEST_FILES = ["**/*.test.{ts,tsx}", "**/__tests__/**", "**/__test-helpers__/**"];

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
      // <State> wrappers. A 'warn' setting would add 1483 warnings (measured
      // 2026-09-20, 343 files) that nothing would act on: the `lint` script CI
      // runs (.github/workflows/ci.yml:426,
      // .github/workflows/stacked-pr-fastlane.yml:391) does NOT pass
      // `--max-warnings 0`, and `lint:strict`, which does, is invoked nowhere.
      // So 'warn' here would not be ENFORCED — see the `error` reasoning below.
      // Flip this to 'error' once the per-page annotation pass lands.
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
      // progressively annotated" and has stayed off ever since across 1483
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
    // Catch-site discard guard — see CATCH_SITE_DISCARD_SELECTORS and
    // CATCH_SITE_CAST_SELECTORS above. Production files get both lists.
    files: ["src/**/*.{ts,tsx}"],
    ignores: TEST_FILES,
    rules: {
      "no-restricted-syntax": [
        "error",
        ...CATCH_SITE_DISCARD_SELECTORS,
        ...CATCH_SITE_CAST_SELECTORS,
      ],
    },
  },
  {
    // Test files: the ternary arms only (a test's cast on an Error it provoked is correct).
    files: TEST_FILES.map((g) => `src/${g}`),
    rules: {
      "no-restricted-syntax": ["error", ...CATCH_SITE_DISCARD_SELECTORS],
    },
  },
  {
    // Population-name guard — see TERMINAL_POPULATION_NAME_SELECTORS above.
    // Spreads the catch-site lists too: flat config REPLACES this rule's
    // options, so omitting them would switch the catch-site guard off under
    // src/components/terminal/** (pinned by
    // src/lib/eslintConfig.catchSiteGuard.test.ts). Same production/test split.
    files: ["src/components/terminal/**/*.{ts,tsx}"],
    ignores: TEST_FILES,
    rules: {
      "no-restricted-syntax": [
        "error",
        ...TERMINAL_POPULATION_NAME_SELECTORS,
        ...CATCH_SITE_DISCARD_SELECTORS,
        ...CATCH_SITE_CAST_SELECTORS,
      ],
    },
  },
  {
    files: TEST_FILES.map((g) => `src/components/terminal/${g}`),
    rules: {
      "no-restricted-syntax": [
        "error",
        ...TERMINAL_POPULATION_NAME_SELECTORS,
        ...CATCH_SITE_DISCARD_SELECTORS,
      ],
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
