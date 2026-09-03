---
name: page-health
description: Run a holistic page health diagnostic on any UI Bridge-connected app. Detects empty content areas, broken layouts, stuck loading states, error signals, and visual anomalies by analyzing element positions, types, and text content. Use when checking if a page looks normal, after restarts, or as a first step in any UI debugging.
user-invocable: true
---

# Page Health Diagnostic

Assess the health of a UI page holistically — the way a human would at a glance.

This is a built-in UI Bridge endpoint: `POST /ui-bridge/control/page-health`

## What It Checks

| Check | What it detects | Severity |
|-------|----------------|----------|
| **Spatial coverage** | What % of the viewport has content | CRITICAL if <15%, WARNING if <30% |
| **Content area empty** | Right side of viewport has no elements (sidebar-only) | CRITICAL |
| **Layout regions** | Sidebar / header / content element distribution | CRITICAL if content=0 |
| **Element diversity** | Whether the page has varied element types or just nav buttons | WARNING if nav-only |
| **Error signals** | Error messages in non-navigation text content | CRITICAL |
| **Loading signals** | Stuck loading/spinner indicators (text + CSS classes) | WARNING |
| **Empty state signals** | "No data", "No results" indicators | WARNING |
| **Interactive readiness** | Disabled controls, pointer-events:none | WARNING if >50% disabled |
| **Visual anomalies** | Zero-size elements, off-screen visible elements | WARNING |

## Single-Column Layout Exception (IMPORTANT)

The `spatial_coverage` and `content_area_empty` checks compare left-half vs right-half
coverage on the assumption that a healthy page has both a sidebar (left) and a content
area (right). **This assumption is wrong for single-column layouts** (e.g., the runner's
Processes page, a centered dashboard, a full-bleed form).

**When interpreting a CRITICAL `spatial_coverage` or `content_area_empty` finding,
first determine whether the page is single-column:**

A page is **single-column** if any of these are true:
- One dominant content stripe spans >70% of the viewport horizontal area
  (e.g., heatmap shows `####################` rows or `..##########..` rows
  with no sidebar/content gap pattern)
- The heatmap shows no consistent vertical empty band separating left and right halves
  (i.e., no recurring `..` gap between sidebar and content)
- `layout_regions.sidebar` is 0 or near-0 AND `layout_regions.content` > 0

**If single-column:** demote `spatial_coverage` and `content_area_empty` findings —
right=2% is expected when there is no right column. Only treat these as CRITICAL if
the *total* coverage is also low (<15%) AND `layout_regions.content` is 0.

**If multi-column** (sidebar visible in heatmap as `##.................` on multiple rows
AND `layout_regions.sidebar` > 0): keep the original CRITICAL severity — right=2% means
the main content failed to render.

This exception lives in interpretation (here), not in the endpoint — the endpoint reports
raw coverage numbers; the LLM must apply the layout-aware gate before escalating.

## How To Use

### From curl (any consumer)

```bash
# Runner
curl -s -X POST http://127.0.0.1:9876/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}'

# Web frontend
curl -s -X POST http://localhost:3001/api/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}'

# A paired device (phone / app) — hit ITS OWN endpoint directly
curl -s -X POST http://<device-ip>:8087/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}'
```

> **Targeting a device:** page-health is element-data-only (it calls
> `discover` internally; no screenshot, no frame pipeline). Unlike the
> pixel skills (`/visual-check`, `/visual-audit`), it takes **no `target`
> field** — to assess a device's page, POST directly to that device's own
> `control/page-health` endpoint (each UI Bridge server, including the
> phone's native server on `:8087`, exposes it). The runner is not in the
> loop, so there is nothing to thread a `target` through.

### From TypeScript (SDK)

```typescript
import { diagnosePageHealth } from '@qontinui/ui-bridge-server';

// From discover results
const report = diagnosePageHealth(elements);
console.log(report.summary); // "CRITICAL" | "WARNING" | "OK"
report.findings.forEach(f => console.log(`${f.severity}: ${f.check} - ${f.detail}`));
```

## Response Format

```json
{
  "success": true,
  "data": {
    "summary": "CRITICAL",
    "element_count": 38,
    "visible_count": 38,
    "findings": [
      {
        "check": "spatial_coverage",
        "severity": "CRITICAL",
        "detail": "Elements cover 9% of viewport. Left=18%, Right=0%",
        "data": { "coverage_pct": 9.0, "left_half_pct": 18.0, "right_half_pct": 0.0 }
      }
    ],
    "heatmap": [
      "##..................",
      "##..................",
      "##.................."
    ]
  }
}
```

## Interpreting the Heatmap

The 20x20 viewport heatmap shows element distribution:

**Broken page** (sidebar only, empty content area):
```
##..................
##..................
##..................
```

**Healthy page** (sidebar + populated content area):
```
####################
###.....############
###.....############
```

## When To Use

- After restarting the runner or web app — quick sanity check
- As the first step in any `/ufix` or `/debug` session
- In automation workflows as a health gate
- When the user says something "looks wrong" but hasn't described what

## How It Works

The endpoint calls discover internally, then analyzes the element data server-side:

1. Builds a 20x20 viewport coverage grid from element `normalizedRect` positions
2. Classifies elements into layout regions (sidebar/header/content) by center position
3. Scans `textContent` and CSS classes for error/loading/empty signals (filtering out nav elements to avoid false positives)
4. Checks interactive element states (enabled, pointer-events)
5. Returns structured findings with severity levels and an ASCII heatmap

No screenshots, no browser, no visual model — just the structured element data the SDK already provides.
