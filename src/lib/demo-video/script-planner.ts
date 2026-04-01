/**
 * Script Planner
 *
 * Uses AI to generate a DemoScript from a page spec and its registered UI Bridge elements.
 */

import type { SpecConfig } from "@/lib/spec-prompt-builder";
import type { DemoScript } from "./types";
import { getApiBase } from "@/lib/runner-api";

// =============================================================================
// Element Fetching
// =============================================================================

interface RegisteredElement {
  id: string;
  type: string;
  label: string;
  actions: string[];
}

export async function fetchRegisteredElements(): Promise<RegisteredElement[]> {
  try {
    const resp = await fetch(`${getApiBase()}/ui-bridge/control/elements`);
    const data = await resp.json();
    if (data.success && Array.isArray(data.data)) {
      return data.data;
    }
    return data.data?.elements ?? [];
  } catch {
    return [];
  }
}

// =============================================================================
// Prompt Building
// =============================================================================

function buildScriptPlanningPrompt(
  spec: SpecConfig,
  elements: RegisteredElement[],
  focusAreas?: string[],
): string {
  const specDescription = spec.description || "No description available";
  const pageUrl = spec.metadata?.pageUrl || "unknown";
  const component = spec.metadata?.component || "unknown";

  const groupSummaries = spec.groups
    .map((g) => {
      const assertions = g.assertions
        .filter((a) => a.enabled !== false)
        .map((a) => `    - ${a.description}`)
        .join("\n");
      return `  ## ${g.name}\n  ${g.description}\n${assertions}`;
    })
    .join("\n\n");

  const elementList = elements
    .map(
      (el) =>
        `  - id: "${el.id}" | type: ${el.type} | label: "${el.label}" | actions: [${el.actions.join(", ")}]`,
    )
    .join("\n");

  const focusSection = focusAreas?.length
    ? `\n\nFOCUS AREAS (prioritize these in the demo):\n${focusAreas.map((f) => `- ${f}`).join("\n")}`
    : "";

  return `You are a demo video script planner for a software product. Given a page specification and its interactive elements, produce a compelling demo script.

PAGE: ${component}
URL: ${pageUrl}
DESCRIPTION: ${specDescription}

PAGE FEATURES:
${groupSummaries}

REGISTERED INTERACTIVE ELEMENTS:
${elementList}
${focusSection}

INSTRUCTIONS:
- Start with a brief intro narration (what the page is, what problem it solves)
- Demonstrate the most important features first
- Use realistic example data (domain-appropriate values, not "test" or "foo")
- Include both happy path and one edge case (e.g., empty search, no results)
- Keep each step to 5-10 seconds of screen time
- Total video: 30-90 seconds per page
- Write narration in present tense, second person ("You can search the timeline by typing...")
- Only reference element IDs that exist in the REGISTERED INTERACTIVE ELEMENTS list
- Include waitForIdle actions after navigation and major state changes
- Default pauseAfterMs to 2000 unless a step needs more or less visual breathing room

VISUAL SUGGESTIONS:
For steps where an abstract concept would benefit from a generated visual (architecture diagrams, data flow, before/after comparisons), add a "visualSuggestion" field with a detailed image generation prompt. These are suggestions — a human will pre-generate the images and wire them in. Good candidates:
- Intro steps: a hero illustration of the concept the page addresses
- Architecture explanations: diagrams showing data flow or system topology
- Transition moments: "before vs after" comparisons
Do NOT add visualSuggestion to every step — only where a visual genuinely helps explain something that screen recording alone cannot convey.

You may also include a "visual" action type for steps that should display a pre-generated image/video overlay:
  { "type": "visual", "filePath": "PLACEHOLDER", "durationMs": 5000, "caption": "Optional caption" }
Use "PLACEHOLDER" for filePath — the user will replace it with a real path to a pre-generated asset. Only include visual actions when you also provide a visualSuggestion for what that image should depict.

OUTPUT FORMAT (strict JSON, no markdown fencing):
{
  "title": "Page Name Demo",
  "description": "One-line description of what the video shows",
  "targetPage": "${pageUrl}",
  "estimatedDurationSecs": <number>,
  "steps": [
    {
      "id": "step-1",
      "narration": "Narration text for this step",
      "actions": [
        { "type": "navigate", "target": "${pageUrl}" },
        { "type": "waitForIdle", "timeout": 5000 }
      ],
      "pauseAfterMs": 2000,
      "visualSuggestion": "A clean hero illustration showing a searchable timeline with colorful event cards flowing left to right, representing captured automation history"
    },
    {
      "id": "step-2",
      "narration": "Next step narration",
      "actions": [
        { "type": "click", "elementId": "some-element-id" }
      ],
      "pauseAfterMs": 2000,
      "highlight": { "elementId": "some-element-id", "type": "pulse" }
    }
  ]
}

Respond ONLY with the JSON object. No explanation, no markdown.`;
}

// =============================================================================
// AI Script Generation
// =============================================================================

export async function planDemoScript(
  spec: SpecConfig,
  elements: RegisteredElement[],
  focusAreas?: string[],
): Promise<DemoScript> {
  const prompt = buildScriptPlanningPrompt(spec, elements, focusAreas);

  const resp = await fetch(`${getApiBase()}/ai/prompt`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      prompt,
      model_preference: "sonnet",
      max_tokens: 4096,
      temperature: 0.3,
    }),
  });

  if (!resp.ok) {
    throw new Error(`AI script planning failed: ${resp.status} ${resp.statusText}`);
  }

  const data = await resp.json();
  const text: string = data.data?.response ?? data.response ?? "";

  // Extract JSON from response (handle potential markdown fencing)
  const jsonMatch = text.match(/\{[\s\S]*\}/);
  if (!jsonMatch) {
    throw new Error("AI response did not contain valid JSON");
  }

  const script: DemoScript = JSON.parse(jsonMatch[0]);

  // Validate structure
  if (!script.title || !Array.isArray(script.steps) || script.steps.length === 0) {
    throw new Error("AI generated an invalid demo script (missing title or steps)");
  }

  return script;
}
