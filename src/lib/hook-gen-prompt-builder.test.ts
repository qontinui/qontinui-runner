/**
 * Tests for buildHookGenPrompt's React Native / Expo Router branch.
 *
 * Adds a regression gate so the prompt always teaches the AI about
 * `Pressable`, `TextInput`, `expo-router`, `useUIComponent` for
 * screen-level registrations, and the `measureInWindow` measurement
 * gotcha. Without these, hook generation against an Expo project
 * produces near-empty output (the integrator only emits a heading
 * registration and bails).
 *
 * Also asserts that the existing web (`react`) branch does NOT contain
 * any of the RN-only guidance — proves the framework dispatch routes
 * correctly.
 */

import { readFileSync } from "node:fs";
import ts from "typescript";
import { describe, it, expect } from "vitest";
import {
  buildHookRegenPrompt,
  buildHookGenPrompt,
  ALL_HOOK_CATEGORIES,
  type HookCategory,
  type ProjectAnalysisForHooks,
} from "./hook-gen-prompt-builder";

const ALL_CATEGORIES: HookCategory[] = [...ALL_HOOK_CATEGORIES];

const EXPO_ANALYSIS: ProjectAnalysisForHooks = {
  framework: "expo_router",
  project_path: "/x/qontinui-mobile",
};

const REACT_ANALYSIS: ProjectAnalysisForHooks = {
  framework: "react",
  project_path: "/x/some-web-app",
};

describe("buildHookGenPrompt — Expo Router / React Native branch", () => {
  const prompt = buildHookGenPrompt(EXPO_ANALYSIS, ALL_CATEGORIES);

  it("mentions Pressable as a host component to wrap", () => {
    expect(prompt.toLowerCase()).toContain("pressable");
  });

  it("mentions TextInput as a host component to wrap", () => {
    expect(prompt).toContain("TextInput");
  });

  it("references expo-router as the routing source", () => {
    expect(prompt).toContain("expo-router");
  });

  it("recommends useUIComponent for screen-level registrations", () => {
    // Per the issue: prefer useUIComponent for screens with form state +
    // multiple actions over per-element registration.
    expect(prompt).toContain("useUIComponent");
  });

  it("calls out the measureInWindow measurement gotcha", () => {
    expect(prompt).toMatch(/measureInWindow/);
  });

  it("teaches the bridgeProps + ref + onLayout three-attach pattern", () => {
    expect(prompt).toContain("bridgeProps");
    expect(prompt).toContain("onLayout");
  });

  it("uses the native SDK import path", () => {
    expect(prompt).toContain("@qontinui/ui-bridge-native");
  });

  it("warns against web-only routing hooks", () => {
    // The RN branch should explicitly tell the AI not to reach for
    // react-router-dom or next/navigation.
    expect(prompt).toMatch(/useLocation|next\/navigation/);
  });

  it("instructs to skip keyboard shortcuts for mobile-only screens", () => {
    expect(prompt.toLowerCase()).toContain("keyboard");
    // The category copy explicitly tells the AI not to fabricate shortcuts.
    expect(prompt).toMatch(/do not generate|do not fabricate|do NOT/i);
  });

  it("explains the Expo file-based route mapping", () => {
    // app/foo.tsx → /foo, app/(tabs)/bar.tsx → /(tabs)/bar, app/[id].tsx → /:id
    expect(prompt).toMatch(/app\/.+\.tsx/);
    expect(prompt).toMatch(/\[id\]/);
    expect(prompt).toMatch(/\(tabs\)/);
  });
});

describe("buildHookGenPrompt — react (web) branch is unchanged", () => {
  const prompt = buildHookGenPrompt(REACT_ANALYSIS, ALL_CATEGORIES);

  it("does NOT include the RN-only native package import", () => {
    expect(prompt).not.toContain("@qontinui/ui-bridge-native");
  });

  it("does NOT include the RN-only measurement gotcha", () => {
    expect(prompt).not.toContain("measureInWindow");
  });

  it("does NOT mention bridgeProps (web pattern uses refs, not bridgeProps)", () => {
    expect(prompt).not.toContain("bridgeProps");
  });

  it("does NOT mention expo-router", () => {
    expect(prompt).not.toContain("expo-router");
  });

  it("still uses the existing React Router guidance (regression check)", () => {
    expect(prompt).toContain("react-router-dom");
  });
});

describe("buildHookGenPrompt — framework dispatch normalization", () => {
  it("routes 'react_native' to the RN branch", () => {
    const prompt = buildHookGenPrompt({ framework: "react_native", project_path: "/x" }, [
      "components",
    ]);
    expect(prompt).toContain("@qontinui/ui-bridge-native");
    expect(prompt).toContain("TextInput");
  });

  it("routes 'expo-router' (hyphen) to the RN branch", () => {
    const prompt = buildHookGenPrompt({ framework: "expo-router", project_path: "/x" }, [
      "components",
    ]);
    expect(prompt).toContain("@qontinui/ui-bridge-native");
  });

  it("routes 'Expo Router' (space + caps) to the RN branch", () => {
    const prompt = buildHookGenPrompt({ framework: "Expo Router", project_path: "/x" }, [
      "components",
    ]);
    expect(prompt).toContain("@qontinui/ui-bridge-native");
  });

  it("does not route 'next.js' to the RN branch", () => {
    const prompt = buildHookGenPrompt({ framework: "next.js", project_path: "/x" }, [
      "route-awareness",
    ]);
    expect(prompt).not.toContain("@qontinui/ui-bridge-native");
    // Existing Next.js guidance should still be present.
    expect(prompt).toContain("next/navigation");
  });
});

describe("buildHookGenPrompt — RN smoke test reads coherently", () => {
  it("produces a non-trivial prompt for components + page-context + route-awareness", () => {
    const prompt = buildHookGenPrompt(EXPO_ANALYSIS, [
      "components",
      "route-awareness",
      "page-context",
    ]);
    // Sanity: non-trivial length and contains structural anchors.
    expect(prompt.length).toBeGreaterThan(2000);
    expect(prompt).toContain("## Categories to Generate");
    // All three categories are represented under their RN headings.
    expect(prompt).toContain("Route Awareness (`useRouteAwareness`) — React Native");
    expect(prompt).toContain("Page Context — React Native / Expo Router (not available)");
    expect(prompt).toContain("Component Actions");
  });
});

/**
 * Plan 2026-10-09-journey-ledger-stores-a-concrete-url-path-as-a-route-pattern:
 * every integrated app is generated from this prompt, so it must teach a route
 * PATTERN derived from the router's params (asserted with `patternSource:
 * "router"`), never the concrete pathname — which carries user input and leaked
 * verbatim into the journey ledger.
 */
describe("buildHookGenPrompt — route patterns never come from the pathname", () => {
  // The framework keys the selector actually dispatches on (`nextjs` /
  // `next_js` for NEXTJS_GUIDANCE — "next.js" selects nothing).
  const frameworks = ["nextjs", "react", "expo_router"];

  for (const framework of frameworks) {
    const prompt = buildHookGenPrompt({ framework, project_path: "/x" }, ["route-awareness"]);

    it(`${framework}: asserts patternSource router`, () => {
      expect(prompt).toContain("patternSource: 'router'");
    });

    it(`${framework}: never generates a pathname as the pattern`, () => {
      // Code only: prose that names the anti-pattern in backticks is allowed.
      expect(prompt).not.toMatch(/(?<!`)pattern:\s*(location\.)?(pathname|usePathname\(\))(?!`)/);
      expect(prompt).not.toMatch(/pattern: matches\[/);
      expect(prompt).not.toContain("routeStack: matches.map(m => m.pathname)");
      expect(prompt).not.toContain("matched: matches.length > 0");
    });
  }

  it("nextjs guidance teaches the not-found signal", () => {
    const prompt = buildHookGenPrompt({ framework: "nextjs", project_path: "/x" }, [
      "route-awareness",
    ]);
    expect(prompt).toContain("### Next.js App Router Integration");
    expect(prompt).toContain("routePatternFromParams(pathname, params, { matched: true })");
    expect(prompt).toContain("useRouteUnmatchedSignal()");
    expect(prompt).toContain("<RouteUnmatchedContext.Provider value={unmatched}>");
    expect(prompt).toContain("{ unmatched },");
    expect(prompt).toContain("app/not-found.tsx");
    expect(prompt).toMatch(/\/\/ app\/RouteAwareness\.tsx[^\n]*\n'use client';\n/);
    expect(prompt).toContain("useMarkRouteUnmatched();");
  });

  it('react router guidance requires a path="*" route marking the signal', () => {
    const prompt = buildHookGenPrompt({ framework: "react", project_path: "/x" }, [
      "route-awareness",
    ]);
    expect(prompt).toContain('A `path="*"` route is REQUIRED.');
    expect(prompt).toContain("useRouteUnmatchedSignal()");
    expect(prompt).toContain("useMarkRouteUnmatched();");
    expect(prompt).toContain("templates the path as `[...*]`");
  });

  it("expo guidance teaches the SDK route helpers, not a hand-built pattern", () => {
    const prompt = buildHookGenPrompt({ framework: "expo_router", project_path: "/x" }, [
      "route-awareness",
    ]);
    const start = prompt.indexOf("Route Awareness (`useRouteAwareness`) — React Native");
    expect(start).toBeGreaterThanOrEqual(0);
    const end = prompt.indexOf("\n### ", start + 1);
    const section = prompt.slice(start, end === -1 ? undefined : end);
    for (const helper of ["expoRouterPattern", "expoRouterRouteStack", "isExpoRouterUnmatched"]) {
      expect(section).toContain(`  ${helper},\n`);
    }
    expect(section).toContain("pattern: expoRouterPattern(segments),");
    expect(section).toContain("routeStack: expoRouterRouteStack(segments),");
    expect(section).toContain("{ unmatched: isExpoRouterUnmatched(segments) },");
    expect(section).not.toContain("segments.join('/')");
    expect(section).not.toContain("useLocalSearchParams()\n");
    // The SDK DOES export the hook: the old "0.6.15 does not" claim is false.
    expect(prompt).not.toMatch(/does not\)?;? if it does not, skip/);
    expect(prompt).not.toContain("0.6.15");
    expect(section).toContain("`@qontinui/ui-bridge-native` 0.6.16 and later");
    // The section's code imports no DOM barrel (prose may name it as a warning).
    expect(section).not.toMatch(/^import .* from '@qontinui\/ui-bridge\/react'/m);
    expect(section).not.toContain("routePatternFromParams(");
  });
});

/**
 * The runner slugs `pageContext.name` into the journey ledger's `pageLabel`
 * (`state_discovery/capture.rs`, `spec_api/slug.rs`), so `name: pathname` on
 * `/search/<secret>` stores `search-<secret>`. The contract forbids deriving
 * `pageLabel` from the pathname in any form: the prompt must teach a FIXED
 * fallback name.
 */
describe("buildHookGenPrompt — page names never come from the pathname", () => {
  const frameworks = ["nextjs", "next_js", "react", "expo_router", "react_native", "vue"];

  for (const framework of frameworks) {
    const prompt = buildHookGenPrompt({ framework, project_path: "/x" }, [
      "route-awareness",
      "page-context",
    ]);

    it(`${framework}: no page name is the pathname`, () => {
      expect(prompt).not.toMatch(/name:\s*(location\.)?pathname/);
      expect(prompt).not.toMatch(/name:\s*usePathname\(\)/);
    });

    if (!["expo_router", "react_native"].includes(framework)) {
      it(`${framework}: teaches the fixed fallback name`, () => {
        expect(prompt).toContain("{ name: 'Unknown', section: 'unknown' }");
      });
    }
  }

  it("react native teaches NO page context: the native SDK has none", () => {
    for (const framework of ["expo_router", "react_native"]) {
      const prompt = buildHookGenPrompt({ framework, project_path: "/x" }, [
        "route-awareness",
        "page-context",
        "state-machine",
      ]);
      // ui-bridge-native exports no usePageContext, and its snapshot page
      // carries `route` only (NativeSnapshotPageContext). No CODE in the
      // prompt — any fenced block, including the shared output-format
      // example — may mention it at all; prose may only warn against it.
      const codeBlocks = prompt.match(/```[\s\S]*?```/g) ?? [];
      expect(codeBlocks.length).toBeGreaterThan(0);
      for (const block of codeBlocks) {
        expect(block).not.toMatch(/usePageContext/);
      }
      expect(prompt).not.toMatch(/###\s*usePageContext\(/);
      expect(prompt).not.toMatch(/usePageContext\(\)\s*deriving/);
      expect(prompt).not.toContain("next/navigation';");
      expect(prompt).toContain("a native snapshot's `page` carries route info only");
      expect(prompt).toContain("Generate no page-context call");
    }
  });

  it("the web output format still shows the page-context hook", () => {
    const prompt = buildHookGenPrompt({ framework: "nextjs", project_path: "/x" }, [
      "page-context",
    ]);
    expect(prompt).toContain("### usePageContext(context: DeveloperPageContext): void");
    expect(prompt).toContain("`usePageContext()` deriving name/section from current route");
  });
});

/**
 * Plan 2026-10-09-journey-ledger-stores-a-concrete-url-path-as-a-route-pattern,
 * round-4 review: no template's own `.trim()` may leak into the prompt text.
 */
describe("buildHookGenPrompt — no source text leaks into the prompt", () => {
  for (const framework of ["nextjs", "react", "expo_router", "react_native", "vue"]) {
    it(`${framework}: no literal .trim() in the prompt`, () => {
      const prompt = buildHookGenPrompt({ framework, project_path: "/x" }, ALL_HOOK_CATEGORIES);
      expect(prompt).not.toContain("})`.trim()");
      expect(prompt).not.toContain("`.trim();");
    });
  }
});

/**
 * The value exports of `@qontinui/ui-bridge-native` that a hook prompt may
 * import, pinned from ui-bridge origin/main d84bfa6
 * `packages/ui-bridge-native/src/index.ts` (React, route-awareness and
 * annotation exports). A name outside this list is not exported by the native
 * package — `useUIState`, `useUITransition`, `usePageContext`,
 * `useKeyboardShortcuts` and `useUndoRedo` are web hooks.
 */
const NATIVE_IMPORTABLE = new Set([
  "UIBridgeNativeProvider",
  "useUIBridgeNative",
  "useUIBridgeNativeOptional",
  "useUIBridgeNativeRequired",
  "useUIElement",
  "useUIElementWithProps",
  "useUIComponent",
  "useUIComponentAction",
  "useUIBridge",
  "useUIBridgeRequired",
  "useUIBridgeModal",
  "useRouteAwareness",
  "expoRouterPattern",
  "expoRouterRouteStack",
  "reactNavigationPattern",
  "isExpoRouterUnmatched",
  "useUIBridgeToast",
  "useToastRecorder",
  "State",
  "TransitionTo",
]);

describe("buildHookGenPrompt — React Native code imports only what ui-bridge-native exports", () => {
  for (const framework of ["expo_router", "react_native"]) {
    it(`${framework}: every name imported from @qontinui/ui-bridge-native is exported by it`, () => {
      const prompts = [
        buildHookGenPrompt({ framework, project_path: "/x" }, ALL_HOOK_CATEGORIES),
        buildHookRegenPrompt({ framework, project_path: "/x" }, ALL_HOOK_CATEGORIES, "// existing"),
      ];
      let imports = 0;
      for (const prompt of prompts) {
        for (const block of prompt.match(/```[\s\S]*?```/g) ?? []) {
          for (const m of block.matchAll(
            /import\s*(?:type\s*)?\{([^}]*)\}\s*from\s*['"]@qontinui\/ui-bridge-native['"]/g,
          )) {
            for (const raw of m[1].split(",")) {
              const name = raw
                .trim()
                .replace(/^type\s+/, "")
                .split(/\s+as\s+/)[0];
              if (!name) continue;
              imports += 1;
              expect(
                NATIVE_IMPORTABLE.has(name),
                `${name} is not exported by ui-bridge-native`,
              ).toBe(true);
            }
          }
        }
      }
      expect(imports).toBeGreaterThan(0);
    });
  }

  it("the react native regen prompt removes an existing usePageContext", () => {
    const regen = buildHookRegenPrompt(
      { framework: "expo_router", project_path: "/x" },
      ["route-awareness"],
      "import { usePageContext } from '@qontinui/ui-bridge/react';",
    );
    expect(regen).toContain("**Remove every page-context hook from the existing code**");
    const web = buildHookRegenPrompt(
      { framework: "nextjs", project_path: "/x" },
      ["route-awareness"],
      "",
    );
    expect(web).not.toContain("Remove every page-context hook");
  });
});

/**
 * Round-5 review: EVERY hook or ui-bridge export a React Native prompt names —
 * in a code fence or in an inline backtick — must be one
 * `@qontinui/ui-bridge-native` exports (NATIVE_IMPORTABLE, plus the undo
 * tracker), or an explicit React / React Native / third-party name. The only
 * place a non-native ui-bridge name may appear is the sentence that says the
 * native package "does NOT export" it.
 */
const NATIVE_NAMED_OK = new Set([...NATIVE_IMPORTABLE, "UndoTracker", "DeclaredUndoState"]);

/** React, React Native, Expo Router, React Navigation and app-side names. */
const NOT_UI_BRIDGE = new Set([
  "useState",
  "useEffect",
  "useMemo",
  "useCallback",
  "useRef",
  "useContext",
  "useReducer",
  "useLayoutEffect",
  "useRouter",
  "usePathname",
  "useSegments",
  "useLocalSearchParams",
  "useGlobalSearchParams",
  "useNavigation",
  "useNavigationContainerRef",
  "useNavigationState",
  "useFocusEffect",
  "usePrompts",
  "useLocation", // react-router-dom, named only to say it does not apply
]);

/** ui-bridge names that are not hooks, so `use[A-Z]` alone would miss them. */
const UI_BRIDGE_NON_HOOKS = [
  "routePatternFromParams",
  "RouteUnmatchedContext",
  "registerIntent",
  "UIBridgeProvider",
  "UIBridgeNativeProvider",
  "State",
  "TransitionTo",
  "UndoTracker",
  "expoRouterPattern",
  "expoRouterRouteStack",
  "isExpoRouterUnmatched",
  "reactNavigationPattern",
];

function namedApis(prompt: string): Set<string> {
  // The one sanctioned place for a non-native name: a "does NOT export" sentence.
  const scanned = prompt.replace(/[^.\n]*does\s+NOT\s+export[^.]*\./g, "");
  // The WHOLE text: prose names a hook as surely as a fence does.
  const spans = [scanned];
  const names = new Set<string>();
  for (const span of spans) {
    for (const m of span.matchAll(/\b(use[A-Z]\w*)\b/g)) names.add(m[1]);
    for (const n of UI_BRIDGE_NON_HOOKS) {
      if (new RegExp(`(^|[^\\w.])${n}\\b`).test(span)) names.add(n);
    }
  }
  return names;
}

describe("buildHookGenPrompt — a React Native prompt names only native APIs", () => {
  for (const framework of ["expo_router", "react_native"]) {
    it(`${framework}: every hook or ui-bridge name is a native export`, () => {
      const prompts = [
        buildHookGenPrompt({ framework, project_path: "/x" }, ALL_HOOK_CATEGORIES),
        buildHookRegenPrompt({ framework, project_path: "/x" }, ALL_HOOK_CATEGORIES, "// existing"),
      ];
      const offenders = new Set<string>();
      for (const prompt of prompts) {
        for (const name of namedApis(prompt)) {
          if (!NATIVE_NAMED_OK.has(name) && !NOT_UI_BRIDGE.has(name)) offenders.add(name);
        }
      }
      expect([...offenders].sort()).toEqual([]);
    });
  }

  it("the scanner sees a web hook in a code fence and in a backtick", () => {
    expect(namedApis("```tsx\nuseUIState({})\n```")).toContain("useUIState");
    expect(namedApis("wire `useUndoRedo` here")).toContain("useUndoRedo");
    expect(namedApis("It does NOT export `useUIState`.")).not.toContain("useUIState");
    expect(namedApis("skipping useUndoRedo in prose")).toContain("useUndoRedo");
  });
});

/**
 * Round-6 review: `useUIElement`'s options are a union over `type` — for
 * `'button'` and `'pressable'`, `handlers.onPress` is REQUIRED
 * (ui-bridge-native d84bfa6 `react/useUIElement.ts`
 * `UseUIElementOptionsHandlersRequired`). Every such call in a React Native
 * code fence must pass `handlers`.
 */
function useUIElementCalls(code: string): string[] {
  const calls: string[] = [];
  let from = 0;
  for (;;) {
    const start = code.indexOf("useUIElement(", from);
    if (start === -1) return calls;
    let depth = 0;
    let end = start + "useUIElement".length;
    for (; end < code.length; end++) {
      const ch = code[end];
      if (ch === "(" || ch === "{") depth++;
      if (ch === ")" || ch === "}") depth--;
      if (depth === 0) break;
    }
    calls.push(code.slice(start, end + 1));
    from = end + 1;
  }
}

describe("buildHookGenPrompt — React Native press elements pass handlers", () => {
  for (const framework of ["expo_router", "react_native"]) {
    it(`${framework}: every button/pressable useUIElement call passes handlers`, () => {
      const prompt = buildHookGenPrompt({ framework, project_path: "/x" }, ALL_HOOK_CATEGORIES);
      const fences = prompt.match(/```[\s\S]*?```/g) ?? [];
      const pressCalls = fences
        .flatMap(useUIElementCalls)
        .filter((call) => /type:\s*['"](button|pressable)['"]/.test(call));
      expect(pressCalls.length).toBeGreaterThan(0);
      for (const call of pressCalls) {
        expect(call, call).toMatch(/\bhandlers:\s*\{[^}]*\bonPress\b/);
      }
    });
  }

  it("the React Native reference documents handlers as the real type does", () => {
    const prompt = buildHookGenPrompt({ framework: "expo_router", project_path: "/x" }, [
      "components",
    ]);
    expect(prompt).toContain(
      "handlers: { onPress: PressHandler; [key: string]: ((...args: any[]) => any) | undefined };",
    );
    expect(prompt).not.toContain("AnyHandler");
    expect(prompt).toContain("`handlers.onPress` is\nREQUIRED");
  });

  it("a React Native exploration list explores only what its categories generate", () => {
    const prompt = buildHookGenPrompt(
      { framework: "expo_router", project_path: "/x" },
      ALL_HOOK_CATEGORIES,
    );
    const start = prompt.indexOf("### Code exploration steps:");
    const end = prompt.indexOf("### Progress communication");
    expect(prompt.slice(start, end)).not.toMatch(/keyboard/i);
    expect(prompt).toContain("skipping the undo category");
  });
});

/**
 * Round-7 review: a React Native prompt's exploration steps and sample notes
 * are built from the SELECTED categories. Each category alone (and none) must
 * name no unselected category's work.
 */
const RN_CATEGORY_WORDS: Record<HookCategory, RegExp> = {
  "route-awareness": /\broutes?\b|router|navigator|app\//i,
  "page-context": /page context|page name/i,
  "state-machine": /UI states?|modal|bottom sheet|drawer/i,
  components: /major components|exposes|registering it/i,
  "keyboard-shortcuts": /keyboard|shortcut|hotkey/i,
  "undo-redo": /undo|redo/i,
  annotations: /annotat/i,
  intents: /multi-step|journey|intent/i,
};

function rnExplorationSection(categories: HookCategory[]): string {
  const prompt = buildHookGenPrompt({ framework: "expo_router", project_path: "/x" }, categories);
  const start = prompt.indexOf("### Code exploration steps:");
  const notes = prompt.indexOf("### Progress communication", start);
  const end = prompt.indexOf("\n\n", notes);
  return prompt.slice(start, end === -1 ? undefined : end);
}

describe("buildHookGenPrompt — React Native exploration follows the selected categories", () => {
  const cases: HookCategory[][] = [[], ...ALL_HOOK_CATEGORIES.map((c) => [c])];
  for (const selected of cases) {
    it(`[${selected.join(",") || "none"}]: no step or note names an unselected category`, () => {
      const section = rnExplorationSection(selected);
      expect(section).toContain("### Code exploration steps:");
      for (const category of ALL_HOOK_CATEGORIES) {
        if (selected.includes(category)) continue;
        expect(section, `${category} leaked into [${selected.join(",")}]`).not.toMatch(
          RN_CATEGORY_WORDS[category],
        );
      }
    });
  }

  it("a selected category with work gets its step", () => {
    expect(rnExplorationSection(["undo-redo"])).toMatch(/Check for undo\/redo/);
    expect(rnExplorationSection(["intents"])).toMatch(/multi-step flows/);
    expect(rnExplorationSection(["state-machine"])).toMatch(/Identify UI states/);
    expect(rnExplorationSection(["route-awareness"])).toMatch(/Find the routes/);
  });
});

/**
 * Round-7 review: every React Native code fence that imports from
 * `@qontinui/ui-bridge-native` must TYPECHECK under `strict` against the
 * mirrored ui-bridge-native d84bfa6 types (`__fixtures__/...env.d.ts.txt`).
 * A real compile, not a pattern match: `handler: ({ id }: { id: string })`
 * is TS2322 under strictFunctionTypes, and a missing `handlers` on a button
 * is TS2345.
 */
const RN_TYPE_ENV = readFileSync(
  new URL("./__fixtures__/ui-bridge-native-d84bfa6.env.d.ts.txt", import.meta.url),
  "utf8",
);

function typecheck(sources: string[]): string[] {
  const files = new Map<string, string>([["/env.d.ts", RN_TYPE_ENV]]);
  sources.forEach((src, i) => files.set(`/fence${i}.tsx`, src));
  const options: ts.CompilerOptions = {
    strict: true,
    noEmit: true,
    jsx: ts.JsxEmit.Preserve,
    target: ts.ScriptTarget.ES2020,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    types: [],
    lib: ["lib.es2020.d.ts", "lib.dom.d.ts"],
  };
  const host = ts.createCompilerHost(options);
  const base = { ...host };
  // Only the TS default libs come from disk: never node_modules' real types,
  // which would shadow the mirrored ambient modules.
  const fromDisk = (name: string) =>
    !name.includes("/node_modules/") || name.includes("/typescript/lib/");
  host.fileExists = (name) => files.has(name) || (fromDisk(name) && base.fileExists(name));
  host.readFile = (name) => files.get(name) ?? (fromDisk(name) ? base.readFile(name) : undefined);
  host.getSourceFile = (name, languageVersion, ...rest) =>
    files.has(name)
      ? ts.createSourceFile(name, files.get(name)!, languageVersion, true)
      : base.getSourceFile(name, languageVersion, ...rest);
  const program = ts.createProgram([...files.keys()], options, host);
  return ts
    .getPreEmitDiagnostics(program)
    .map(
      (d) =>
        `${d.file?.fileName ?? "?"}: TS${d.code} ${ts.flattenDiagnosticMessageText(d.messageText, " ")}`,
    );
}

function nativeFences(prompt: string): string[] {
  return (prompt.match(/```tsx?\n[\s\S]*?```/g) ?? [])
    .map((f) => f.replace(/^```tsx?\n/, "").replace(/```$/, ""))
    .filter((code) => /from\s+["']@qontinui\/ui-bridge-native["']/.test(code));
}

describe("buildHookGenPrompt — React Native examples typecheck against ui-bridge-native", () => {
  it("the mirrored types catch the defects the examples must not have", () => {
    const errors = typecheck([
      `import { useUIElement, useUIComponent } from '@qontinui/ui-bridge-native';
       useUIElement({ id: 'b', type: 'button', label: 'x' });
       useUIComponent({ id: 'c', name: 'C', actions: [
         { id: 'a', handler: ({ id }: { id: string }) => id },
       ] });`,
    ]);
    expect(errors.some((e) => e.includes("TS2345") || e.includes("TS2322"))).toBe(true);
    expect(errors.length).toBeGreaterThanOrEqual(2);
  });

  for (const framework of ["expo_router", "react_native"]) {
    it(`${framework}: every native code fence compiles under strict`, () => {
      const prompt = buildHookGenPrompt({ framework, project_path: "/x" }, ALL_HOOK_CATEGORIES);
      const fences = nativeFences(prompt);
      // Every native fence, exactly: route awareness (guidance + category),
      // state machine, CreateButton, SearchInput, PromptsScreen, undo, and the
      // output-format example. A fence that drops out of the filter fails here.
      expect(fences).toHaveLength(8);
      expect(fences.some((f) => f.includes("function PromptsScreen"))).toBe(true);
      expect(fences.some((f) => f.includes("function CreateButton"))).toBe(true);
      expect(fences.some((f) => f.includes("function SearchInput"))).toBe(true);
      expect(typecheck(fences)).toEqual([]);
    });
  }
});

/**
 * Round-7 review: every TYPE the React Native API reference names in its code
 * must be a type `@qontinui/ui-bridge-native` exports (ui-bridge d84bfa6
 * `packages/ui-bridge-native/src/index.ts`) or a TS built-in — `AnyHandler`,
 * for one, is module-private there.
 */
const NATIVE_EXPORTED_TYPES = new Set([
  "RouteInfo",
  "NativeElementType",
  "PressNeedingNativeElementType",
  "PressHandler",
  "NativeStandardAction",
  "NativeCustomAction",
  "NativeElementState",
  "UseUIElementOptions",
  "UseUIElementOptionsBase",
  "UseUIElementOptionsHandlersRequired",
  "UseUIElementOptionsHandlersOptional",
  "UseUIElementReturn",
  "UIBridgeProps",
  "UseUIComponentOptions",
  "UseUIComponentReturn",
  "ComponentActionDef",
  "IREffect",
  "StateProps",
  "TransitionToProps",
  "StateAnnotationRequiredElement",
  "DeclaredUndoState",
  "UseRouteAwarenessOptions",
  "NavigationStateLike",
]);
const TS_BUILTIN_TYPES = new Set(["Record", "Promise", "AbortSignal", "Array", "ReadonlyArray"]);

describe("buildHookGenPrompt — the React Native API reference names only exported types", () => {
  it("every PascalCase type in the reference's code is a native export or a built-in", () => {
    const prompt = buildHookGenPrompt(
      { framework: "expo_router", project_path: "/x" },
      ALL_HOOK_CATEGORIES,
    );
    const start = prompt.indexOf("## UI Bridge Native API Reference");
    const end = prompt.indexOf("\n## ", start + 1);
    const reference = prompt.slice(start, end);
    const code = (reference.match(/```typescript\n[\s\S]*?```/g) ?? []).join("\n");
    expect(code.length).toBeGreaterThan(0);
    const names = new Set(
      [...code.replace(/\/\/[^\n]*/g, "").matchAll(/\b([A-Z][A-Za-z0-9]+)\b/g)].map((m) => m[1]),
    );
    const unknown = [...names].filter(
      (n) => !NATIVE_EXPORTED_TYPES.has(n) && !TS_BUILTIN_TYPES.has(n),
    );
    expect(unknown).toEqual([]);
  });
});
