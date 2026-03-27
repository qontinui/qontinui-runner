/**
 * Tour Exporter
 *
 * Exports product tours as JSON, Markdown, or standalone HTML.
 */

import type { ProductTour, ProductTourStep } from "@/types/product-tour";

// =============================================================================
// JSON Export
// =============================================================================

export function exportTourAsJson(tour: ProductTour): string {
  return JSON.stringify(tour, null, 2);
}

// =============================================================================
// Markdown Export
// =============================================================================

export function exportTourAsMarkdown(tour: ProductTour): string {
  const lines: string[] = [
    `# ${tour.title}`,
    "",
    `> ${tour.subtitle}`,
    "",
    `**Audience:** ${tour.targetAudience} | **Duration:** ~${tour.estimatedMinutes} min | **Pages:** ${tour.pages.join(", ")}`,
    "",
    "---",
    "",
  ];

  for (let i = 0; i < tour.steps.length; i++) {
    const step = tour.steps[i];
    lines.push(`### ${i + 1}. ${step.headline}`);
    lines.push("");
    lines.push(step.body);
    if (step.demoAction) {
      lines.push("");
      lines.push(`*Demo: ${describeDemoAction(step.demoAction)}*`);
    }
    lines.push("");
  }

  if (tour.cta) {
    lines.push("---");
    lines.push("");
    lines.push(`**${tour.cta.label}**`);
    if (tour.cta.url) lines.push(`[${tour.cta.url}](${tour.cta.url})`);
  }

  return lines.join("\n");
}

function describeDemoAction(action: ProductTourStep["demoAction"]): string {
  if (!action) return "";
  switch (action.type) {
    case "click":
      return `Click ${action.elementId}`;
    case "type":
      return `Type "${action.value}" into ${action.elementId}`;
    case "select":
      return `Select "${action.value}" in ${action.elementId}`;
    case "navigate":
      return `Navigate to ${action.target}`;
    case "wait":
      return `Pause ${action.ms}ms`;
  }
}

// =============================================================================
// Standalone HTML Export
// =============================================================================

export function exportTourAsHtml(tour: ProductTour): string {
  const stepsHtml = tour.steps
    .map(
      (step, i) => `
      <div class="step" id="step-${i}">
        <div class="step-number">${i + 1}</div>
        <h2>${escapeHtml(step.headline)}</h2>
        <p>${escapeHtml(step.body)}</p>
        ${step.demoAction ? `<div class="demo-badge">${escapeHtml(describeDemoAction(step.demoAction))}</div>` : ""}
      </div>`
    )
    .join("\n");

  const ctaHtml = tour.cta
    ? `<div class="cta">
        ${tour.cta.url ? `<a href="${escapeHtml(tour.cta.url)}">${escapeHtml(tour.cta.label)}</a>` : `<button>${escapeHtml(tour.cta.label)}</button>`}
       </div>`
    : "";

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>${escapeHtml(tour.title)}</title>
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
      background: #0a0a0a;
      color: #e5e5e5;
      min-height: 100vh;
      display: flex;
      flex-direction: column;
      align-items: center;
      padding: 2rem;
    }
    header {
      text-align: center;
      margin-bottom: 3rem;
      max-width: 600px;
    }
    header h1 {
      font-size: 2.5rem;
      font-weight: 700;
      color: #fff;
      margin-bottom: 0.5rem;
    }
    header p {
      font-size: 1.1rem;
      color: #a3a3a3;
    }
    .meta {
      display: flex;
      gap: 1.5rem;
      justify-content: center;
      margin-top: 1rem;
      font-size: 0.85rem;
      color: #737373;
    }
    .steps {
      max-width: 640px;
      width: 100%;
      display: flex;
      flex-direction: column;
      gap: 1.5rem;
    }
    .step {
      background: #171717;
      border: 1px solid #262626;
      border-radius: 12px;
      padding: 1.5rem;
      position: relative;
      transition: border-color 0.2s;
    }
    .step:hover { border-color: #3b82f6; }
    .step-number {
      position: absolute;
      top: -0.75rem;
      left: 1.25rem;
      background: #3b82f6;
      color: #fff;
      width: 1.5rem;
      height: 1.5rem;
      border-radius: 50%;
      display: flex;
      align-items: center;
      justify-content: center;
      font-size: 0.75rem;
      font-weight: 700;
    }
    .step h2 {
      font-size: 1.15rem;
      font-weight: 600;
      color: #fff;
      margin-bottom: 0.5rem;
    }
    .step p {
      font-size: 0.95rem;
      color: #a3a3a3;
      line-height: 1.5;
    }
    .demo-badge {
      margin-top: 0.75rem;
      display: inline-block;
      padding: 0.25rem 0.75rem;
      background: #1e3a5f;
      border: 1px solid #2563eb40;
      border-radius: 6px;
      font-size: 0.8rem;
      color: #60a5fa;
    }
    .cta {
      margin-top: 3rem;
      text-align: center;
    }
    .cta a, .cta button {
      display: inline-block;
      padding: 0.75rem 2rem;
      background: #3b82f6;
      color: #fff;
      border: none;
      border-radius: 8px;
      font-size: 1rem;
      font-weight: 600;
      text-decoration: none;
      cursor: pointer;
      transition: background 0.2s;
    }
    .cta a:hover, .cta button:hover { background: #2563eb; }
  </style>
</head>
<body>
  <header>
    <h1>${escapeHtml(tour.title)}</h1>
    <p>${escapeHtml(tour.subtitle)}</p>
    <div class="meta">
      <span>${tour.estimatedMinutes} min</span>
      <span>${tour.steps.length} steps</span>
      <span>${tour.targetAudience}</span>
    </div>
  </header>
  <div class="steps">
    ${stepsHtml}
  </div>
  ${ctaHtml}
</body>
</html>`;
}

function escapeHtml(str: string): string {
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

// =============================================================================
// Combined Export
// =============================================================================

export interface TourExports {
  json: string;
  markdown: string;
  html: string;
}

export function exportTour(tour: ProductTour): TourExports {
  return {
    json: exportTourAsJson(tour),
    markdown: exportTourAsMarkdown(tour),
    html: exportTourAsHtml(tour),
  };
}
