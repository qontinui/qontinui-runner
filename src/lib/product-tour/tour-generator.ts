/**
 * Tour Generator
 *
 * Uses AI to generate customer-facing product tours from page specs
 * and registered UI Bridge elements.
 */

import type { SpecConfig } from "@/lib/spec-prompt-builder";
import type { ProductTour, TourAudience } from "@/types/product-tour";
import { getApiBase } from "@/lib/runner-api";

// =============================================================================
// Element Fetching (shared with demo-video)
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

const AUDIENCE_CONTEXT: Record<TourAudience, string> = {
  "new-user":
    "A first-time user who has never seen this product. Focus on basics, use simple language, highlight the most common workflows.",
  "power-user":
    "An experienced user discovering advanced features. Focus on efficiency, shortcuts, and powerful capabilities they might not know about.",
  evaluator:
    "A decision-maker evaluating the product. Focus on differentiators, ROI, and competitive advantages. Emphasize what makes this unique.",
};

function buildTourPlanningPrompt(
  spec: SpecConfig,
  elements: RegisteredElement[],
  audience: TourAudience
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
        `  - id: "${el.id}" | type: ${el.type} | label: "${el.label}" | actions: [${el.actions.join(", ")}]`
    )
    .join("\n");

  return `You are generating a customer-facing product tour for a software application.

TARGET AUDIENCE: ${audience}
${AUDIENCE_CONTEXT[audience]}

PAGE: ${component}
URL: ${pageUrl}
VALUE PROPOSITION: ${specDescription}

PAGE FEATURES (from semantic spec groups):
${groupSummaries}

AVAILABLE INTERACTIVE ELEMENTS:
${elementList}

INSTRUCTIONS:
- Generate a tour that SHOWS the feature working, not just describes it
- Each step should spotlight an element and auto-execute a demo action
- Write benefit-focused headlines (not feature names): "Find Anything Instantly" not "Search Input"
- Body text: 1-2 sentences max, focus on user value, no technical jargon
- Use realistic demo data for type/select actions (domain-appropriate, not "test" or "foo")
- 5-8 steps total, covering the page's key value propositions
- Flow: context/intro → demonstrate core feature → show supporting features → wrap up
- Only reference element IDs from the AVAILABLE INTERACTIVE ELEMENTS list
- For "auto" transition: step advances after demo action completes (add transitionDelayMs: 3000-5000)
- For "click" transition: user clicks Next (use for important moments worth pausing on)
- For "timer" transition: step auto-advances after transitionDelayMs (use for observe-only steps)

OUTPUT FORMAT (strict JSON, no markdown fencing):
{
  "id": "tour-${pageUrl.replace(/^\//, "").replace(/\//g, "-") || "page"}",
  "title": "Discover [Feature Name]",
  "subtitle": "One-line value proposition",
  "targetAudience": "${audience}",
  "estimatedMinutes": <number>,
  "pages": ["${pageUrl}"],
  "steps": [
    {
      "id": "step-1",
      "headline": "Benefit-Focused Headline",
      "body": "Short explanation of value to user.",
      "targetElement": {
        "elementId": "element-id-from-list",
        "highlight": "spotlight"
      },
      "demoAction": { "type": "navigate", "target": "${pageUrl}" },
      "transition": "auto",
      "transitionDelayMs": 3000
    },
    {
      "id": "step-2",
      "headline": "See It In Action",
      "body": "Watch as we demonstrate this feature.",
      "targetElement": {
        "elementId": "search-input",
        "highlight": "pulse"
      },
      "demoAction": { "type": "type", "elementId": "search-input", "value": "realistic search query" },
      "transition": "auto",
      "transitionDelayMs": 4000
    }
  ],
  "cta": {
    "label": "Start Using This Feature",
    "action": "navigate-to-page"
  }
}

Respond ONLY with the JSON object. No explanation, no markdown.`;
}

// =============================================================================
// AI Tour Generation
// =============================================================================

export async function generateTour(
  spec: SpecConfig,
  elements: RegisteredElement[],
  audience: TourAudience = "new-user"
): Promise<ProductTour> {
  const prompt = buildTourPlanningPrompt(spec, elements, audience);

  const resp = await fetch(`${getApiBase()}/ai/prompt`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      prompt,
      model_preference: "sonnet",
      max_tokens: 4096,
      temperature: 0.4,
    }),
  });

  if (!resp.ok) {
    throw new Error(`AI tour generation failed: ${resp.status} ${resp.statusText}`);
  }

  const data = await resp.json();
  const text: string = data.data?.response ?? data.response ?? "";

  const jsonMatch = text.match(/\{[\s\S]*\}/);
  if (!jsonMatch) {
    throw new Error("AI response did not contain valid JSON");
  }

  const tour: ProductTour = JSON.parse(jsonMatch[0]);

  if (!tour.title || !Array.isArray(tour.steps) || tour.steps.length === 0) {
    throw new Error("AI generated an invalid tour (missing title or steps)");
  }

  return tour;
}
