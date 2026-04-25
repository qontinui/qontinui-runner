/**
 * Tests for buildRegistrationPrompt — Phase D of the Expo Router integrator
 * plan.
 *
 * Covers the framework-conditional branch added so React Native / Expo Router
 * projects get an RN-shaped MUST-register block (native element types,
 * three-attach hook usage, no useUIComponent), while the existing web path
 * for `react` / `next` is unchanged.
 *
 * The Rust analyzer serializes `Framework::ExpoRouter` as `"expo_router"` and
 * `Framework::ReactNative` as `"react_native"` (snake_case via serde). The
 * prompt builder matches on those exact strings.
 */

import { describe, it, expect } from "vitest";
import {
  buildRegistrationPrompt,
  isReactNativeFramework,
  type InlineRegistration,
} from "./page-analysis-prompt-builder";

const SAMPLE_PAGE_SOURCE = `
import { Pressable, Text, TextInput, View } from 'react-native';

export default function PromptsPage() {
  return (
    <View>
      <Text>Prompts</Text>
      <TextInput placeholder="search" />
      <Pressable onPress={() => {}}>
        <Text>Create</Text>
      </Pressable>
    </View>
  );
}
`;

describe("buildRegistrationPrompt — web path (regression)", () => {
  it("emits the web side-file instructions for framework='react'", () => {
    const prompt = buildRegistrationPrompt(
      SAMPLE_PAGE_SOURCE,
      [],
      "DashboardPage",
      "/dashboard",
      "react",
    );

    // Web import path.
    expect(prompt).toContain('"@qontinui/ui-bridge"');
    // Web hook surface.
    expect(prompt).toContain("useUIComponent");
    expect(prompt).toContain("useUIElement");
    // Web side-file output marker.
    expect(prompt).toContain("src/lib/ui-bridge/pages/");
    // Returns refs (web pattern), no mention of bridgeProps / onLayout.
    expect(prompt).toContain("React.RefCallback");
    expect(prompt).not.toContain("bridgeProps");
    expect(prompt).not.toContain("@qontinui/ui-bridge-native");
  });

  it("emits the web instructions for framework='next' / 'next.js'", () => {
    for (const fw of ["next", "next.js", "nextjs"]) {
      const prompt = buildRegistrationPrompt(
        SAMPLE_PAGE_SOURCE,
        [],
        "DashboardPage",
        "/dashboard",
        fw,
      );
      expect(prompt).toContain('"@qontinui/ui-bridge"');
      expect(prompt).toContain("useUIComponent");
      expect(prompt).not.toContain("@qontinui/ui-bridge-native");
    }
  });
});

describe("buildRegistrationPrompt — RN path (expo_router)", () => {
  const prompt = buildRegistrationPrompt(
    SAMPLE_PAGE_SOURCE,
    [],
    "PromptsPage",
    "/prompts",
    "expo_router",
  );

  it("uses the native package import", () => {
    expect(prompt).toContain("@qontinui/ui-bridge-native");
    // The web import string `"@qontinui/ui-bridge"` should NOT appear as an
    // import statement. (`@qontinui/ui-bridge-native` contains `ui-bridge`
    // as a substring but doesn't match `"@qontinui/ui-bridge"` exactly.)
    expect(prompt).not.toContain('"@qontinui/ui-bridge"');
    expect(prompt).not.toContain("'@qontinui/ui-bridge'");
  });

  it("does not mention useUIComponent", () => {
    expect(prompt).not.toContain("useUIComponent");
  });

  it("instructs the three-attach pattern (ref, onLayout, bridgeProps)", () => {
    expect(prompt).toContain("ref");
    expect(prompt).toContain("onLayout");
    expect(prompt).toContain("bridgeProps");
    expect(prompt).toMatch(/all three/i);
  });

  it("lists the native element type set", () => {
    // Spot-check the native types.
    expect(prompt).toContain("pressable");
    expect(prompt).toContain("touchable");
    expect(prompt).toContain("listItem");
    expect(prompt).toContain("modal");
    expect(prompt).toContain("custom");
  });

  it("forbids web-only element types", () => {
    // These are HTML/web ElementType values that don't exist in
    // NativeElementType. The prompt explicitly forbids them.
    for (const banned of [
      '"link"',
      '"tab"',
      '"slider"',
      '"menuitem"',
      '"disclosure"',
      '"generic"',
      '"dialog"',
      '"select"',
      '"textarea"',
    ]) {
      // The forbidden token may appear in the "do not use" list, but it
      // should not appear as a recommended type. The prompt's "Do NOT use"
      // line lists them; that's expected. We assert it does NOT appear in
      // a positive recommendation context. The simplest robust check: each
      // banned token, if present at all, appears only inside the "Do NOT
      // use" sentence.
      const idx = prompt.indexOf(banned);
      if (idx === -1) continue;
      const window = prompt.slice(Math.max(0, idx - 200), idx);
      expect(window.toLowerCase()).toMatch(/do not use|don't use|forbidden/);
    }
  });

  it("instructs inline output (modified page file), not a side-file", () => {
    // The web path emits to src/lib/ui-bridge/pages/<name>-registrations.tsx.
    // The RN path should NOT use that output location.
    expect(prompt).not.toContain("src/lib/ui-bridge/pages/");
    // It should reference modifying the page component file in place.
    expect(prompt).toMatch(/inline|modified.*page|FILE: app\//i);
  });

  it("uses native standard actions (press, type, toggle, scroll)", () => {
    expect(prompt).toContain('["press"]');
    expect(prompt).toContain('["type", "clear"]');
    expect(prompt).toContain('["toggle"]');
    expect(prompt).toContain('["scroll"]');
  });

  it("preserves inline-registration handling", () => {
    const inlineRegs: InlineRegistration[] = [
      { hook: "useUIElement", id: "prompts-search-input", type: "input", label: "Search prompts" },
    ];
    const promptWithInline = buildRegistrationPrompt(
      SAMPLE_PAGE_SOURCE,
      [],
      "PromptsPage",
      "/prompts",
      "expo_router",
      undefined,
      inlineRegs,
    );
    expect(promptWithInline).toContain("Elements Already Tagged Inline");
    expect(promptWithInline).toContain("prompts-search-input");
    expect(promptWithInline).toMatch(/do not emit|skip/i);
  });
});

describe("buildRegistrationPrompt — RN path normalization", () => {
  it("routes hyphenated 'expo-router' to the RN branch", () => {
    const prompt = buildRegistrationPrompt(
      SAMPLE_PAGE_SOURCE,
      [],
      "PromptsPage",
      "/prompts",
      "expo-router",
    );
    expect(prompt).toContain("@qontinui/ui-bridge-native");
    expect(prompt).not.toContain("useUIComponent");
  });

  it("routes 'Expo Router' (with space and capitalization) to the RN branch", () => {
    const prompt = buildRegistrationPrompt(
      SAMPLE_PAGE_SOURCE,
      [],
      "PromptsPage",
      "/prompts",
      "Expo Router",
    );
    expect(prompt).toContain("@qontinui/ui-bridge-native");
    expect(prompt).not.toContain("useUIComponent");
  });

  it("routes 'react-native' (hyphenated) to the RN branch", () => {
    const prompt = buildRegistrationPrompt(
      SAMPLE_PAGE_SOURCE,
      [],
      "PromptsPage",
      "/prompts",
      "react-native",
    );
    expect(prompt).toContain("@qontinui/ui-bridge-native");
    expect(prompt).not.toContain("useUIComponent");
  });

  it("isReactNativeFramework normalizes separators and case", () => {
    expect(isReactNativeFramework("expo_router")).toBe(true);
    expect(isReactNativeFramework("expo-router")).toBe(true);
    expect(isReactNativeFramework("Expo Router")).toBe(true);
    expect(isReactNativeFramework("EXPO_ROUTER")).toBe(true);
    expect(isReactNativeFramework("react_native")).toBe(true);
    expect(isReactNativeFramework("react-native")).toBe(true);
    expect(isReactNativeFramework("React Native")).toBe(true);

    expect(isReactNativeFramework("react")).toBe(false);
    expect(isReactNativeFramework("next")).toBe(false);
    expect(isReactNativeFramework("next.js")).toBe(false);
    expect(isReactNativeFramework("nextjs")).toBe(false);
    expect(isReactNativeFramework("")).toBe(false);
    expect(isReactNativeFramework("expo")).toBe(false);
  });
});

describe("buildRegistrationPrompt — RN path (react_native)", () => {
  const prompt = buildRegistrationPrompt(
    SAMPLE_PAGE_SOURCE,
    [],
    "PromptsPage",
    "/prompts",
    "react_native",
  );

  it("uses the native package import for bare RN too", () => {
    expect(prompt).toContain("@qontinui/ui-bridge-native");
  });

  it("does not mention useUIComponent for bare RN", () => {
    expect(prompt).not.toContain("useUIComponent");
  });

  it("instructs the three-attach pattern", () => {
    expect(prompt).toContain("onLayout");
    expect(prompt).toContain("bridgeProps");
  });

  it("forbids web-only types for bare RN", () => {
    // Same as above — link/generic/disclosure/dialog must not appear as
    // recommended types.
    for (const banned of ['"link"', '"generic"', '"disclosure"', '"dialog"']) {
      const idx = prompt.indexOf(banned);
      if (idx === -1) continue;
      const window = prompt.slice(Math.max(0, idx - 200), idx);
      expect(window.toLowerCase()).toMatch(/do not use|don't use|forbidden/);
    }
  });
});
