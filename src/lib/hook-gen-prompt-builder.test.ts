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

import { describe, it, expect } from "vitest";
import {
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
    expect(prompt).toContain("Page Context (`usePageContext`) — React Native");
    expect(prompt).toContain("Component Actions");
  });
});
