/**
 * Behaviour tests for HookGenerationPanel's AI-output parsers. Fixtures are
 * shaped like real AI turns: prose around fenced blocks, `// FILE:`,
 * `%% FILE:` and `<!-- FILE: -->` markers.
 */

import { describe, it, expect } from "vitest";

import {
  MAX_JSON_BLOCK_SIZE,
  clusterSpecsByPrefix,
  extractGeneratedFiles,
  extractJsonBlock,
  extractMarkdownFiles,
  extractMermaidFile,
  gatherExplainerInputs,
  type GeneratedFile,
} from "./parse";
import type { ExplainerSpecSummary } from "@/lib/page-analysis-prompt-builder";

describe("extractGeneratedFiles", () => {
  it("extracts every fenced file carrying a // FILE: marker, across language tags", () => {
    const ai = [
      "Here are the hook files.",
      "",
      "```tsx",
      "// FILE: src/lib/ui-bridge/UIBridgeHooks.tsx",
      "export function useRoute() {}",
      "```",
      "",
      "And a plain TS helper:",
      "",
      "```TypeScript",
      "",
      "// FILE: src/lib/ui-bridge/state.ts  ",
      "export const x = 1;",
      "```",
      "",
      "```",
      "// FILE: src/untagged.js",
      "console.log(1);",
      "```",
    ].join("\n");

    const files = extractGeneratedFiles(ai);
    expect(files.map((f) => f.filePath)).toEqual([
      "src/lib/ui-bridge/UIBridgeHooks.tsx",
      "src/lib/ui-bridge/state.ts",
      "src/untagged.js",
    ]);
    expect(files[0].content).toBe(
      "// FILE: src/lib/ui-bridge/UIBridgeHooks.tsx\nexport function useRoute() {}\n",
    );
  });

  it("handles CRLF line endings", () => {
    const ai = "```ts\r\n// FILE: a.ts\r\nconst a = 1;\r\n```";
    const files = extractGeneratedFiles(ai);
    expect(files).toHaveLength(1);
    expect(files[0].filePath).toBe("a.ts");
  });

  it("ignores fenced blocks without a FILE marker", () => {
    expect(extractGeneratedFiles("```ts\nconst a = 1;\n```")).toEqual([]);
  });
});

describe("extractMermaidFile", () => {
  it("extracts the fenced mermaid file and normalizes its marker line", () => {
    const ai = [
      "The architecture diagram:",
      "",
      "```mermaid",
      "%%   FILE: src/specs/dashboard.arch.mmd",
      "flowchart TD",
      "  A --> B",
      "",
      "```",
      "Done.",
    ].join("\n");
    expect(extractMermaidFile(ai)).toEqual({
      filePath: "src/specs/dashboard.arch.mmd",
      content: "%% FILE: src/specs/dashboard.arch.mmd\nflowchart TD\n  A --> B\n",
    });
  });

  it("returns null when there is no mermaid block with a FILE marker", () => {
    expect(extractMermaidFile("```mermaid\nflowchart TD\n```")).toBeNull();
    expect(extractMermaidFile("no diagram here")).toBeNull();
  });
});

describe("gatherExplainerInputs", () => {
  it("parses spec summaries and strips the %% FILE line from arch diagrams", () => {
    const files: GeneratedFile[] = [
      {
        filePath: "src/specs/settings-general.spec.uibridge.json",
        content: JSON.stringify({
          description: "  General settings  ",
          groups: [
            { id: "g1", name: "Theme", description: " pick a theme " },
            { id: "g2" }, // no name → dropped
          ],
        }),
      },
      {
        filePath: "src/specs/settings-general.arch.mmd",
        content: "%% FILE: src/specs/settings-general.arch.mmd\nflowchart TD\n  A-->B\n",
      },
      { filePath: "src/specs/broken.spec.uibridge.json", content: "{not json" },
      { filePath: "src/lib/other.ts", content: "// unrelated" },
    ];
    const { specs, arch } = gatherExplainerInputs(files);
    expect(specs).toEqual([
      {
        specId: "settings-general",
        description: "General settings",
        groups: [{ id: "g1", name: "Theme", description: "pick a theme" }],
      },
    ]);
    expect(Array.from(arch.entries())).toEqual([["settings-general", "flowchart TD\n  A-->B"]]);
  });
});

describe("clusterSpecsByPrefix", () => {
  const spec = (specId: string): ExplainerSpecSummary => ({ specId, description: "", groups: [] });

  it("groups by first token and folds singletons into 'other'", () => {
    const clusters = clusterSpecsByPrefix([
      spec("settings-general"),
      spec("settings-ai"),
      spec("settings-git"),
      spec("runs-active"),
      spec("runs-history"),
      spec("dashboard"),
    ]);
    expect(clusters.map((c) => [c.id, c.name, c.specIds])).toEqual([
      ["settings", "Settings", ["settings-general", "settings-ai", "settings-git"]],
      ["runs", "Runs", ["runs-active", "runs-history"]],
      ["other", "Other", ["dashboard"]],
    ]);
    expect(clusters[0].description).toBe('3 related pages under the "settings" umbrella.');
  });

  it("caps at 7 named clusters and spills the rest into 'other'", () => {
    const specs: ExplainerSpecSummary[] = [];
    for (let i = 0; i < 9; i++) specs.push(spec(`p${i}-a`), spec(`p${i}-b`));
    const clusters = clusterSpecsByPrefix(specs);
    expect(clusters).toHaveLength(8);
    expect(clusters[7].id).toBe("other");
    expect(clusters[7].specIds).toHaveLength(4);
  });
});

describe("extractMarkdownFiles", () => {
  it("splits on <!-- FILE: --> markers without leaking the next marker", () => {
    const ai = [
      "Here is the explainer.",
      "",
      "<!-- FILE: src/specs/explainer/index.md -->",
      "# Overview",
      "",
      "See [settings](./settings.md).",
      "",
      "<!--   FILE: src/specs/explainer/settings.md   -->",
      "# Settings",
      "Body.",
      "",
    ].join("\n");
    expect(extractMarkdownFiles(ai)).toEqual([
      {
        filePath: "src/specs/explainer/index.md",
        content:
          "<!-- FILE: src/specs/explainer/index.md -->\n# Overview\n\nSee [settings](./settings.md).\n",
      },
      {
        filePath: "src/specs/explainer/settings.md",
        content: "<!-- FILE: src/specs/explainer/settings.md -->\n# Settings\nBody.\n",
      },
    ]);
  });

  it("returns nothing when there are no markers", () => {
    expect(extractMarkdownFiles("# Just markdown\n")).toEqual([]);
  });
});

describe("extractJsonBlock", () => {
  it("returns the first valid ```json block, trimmed", () => {
    const ai = 'Spec:\n\n```json\n{ "id": "page-a", "groups": [] }\n```\n';
    expect(extractJsonBlock(ai)).toBe('{ "id": "page-a", "groups": [] }');
  });

  it("skips an invalid ```json block and takes the next valid one", () => {
    const ai = '```json\n{ "id": oops }\n```\n\nCorrected:\n\n```json\n{ "id": "b" }\n```';
    expect(extractJsonBlock(ai)).toBe('{ "id": "b" }');
  });

  it("does not pair a ```json opener with the closing fence of another block", () => {
    const ai = '```ts\nconst a = 1;\n```\n\n```json\n{ "ok": true }\n```';
    expect(extractJsonBlock(ai)).toBe('{ "ok": true }');
  });

  it("falls back to a bare ``` fence holding a JSON object", () => {
    const ai = 'The spec:\n\n```\n{\n  "id": "bare",\n  "groups": []\n}\n```\n';
    expect(extractJsonBlock(ai)).toBe('{\n  "id": "bare",\n  "groups": []\n}');
  });

  it("skips an oversized (> 1 MB) block", () => {
    const big = JSON.stringify({ blob: "x".repeat(MAX_JSON_BLOCK_SIZE) });
    expect(big.length).toBeGreaterThan(MAX_JSON_BLOCK_SIZE);
    expect(extractJsonBlock("```json\n" + big + "\n```")).toBeNull();
    expect(extractJsonBlock("```json\n" + big + '\n```\n\n```json\n{"small":1}\n```')).toBe(
      '{"small":1}',
    );
  });

  it("returns null when no block parses", () => {
    expect(extractJsonBlock("```json\nnot json\n```\n```\n{ nope }\n```")).toBeNull();
    expect(extractJsonBlock("no fences at all")).toBeNull();
  });
});
