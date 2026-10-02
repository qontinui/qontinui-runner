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

## Read `status` FIRST — could-not-see is not a broken page

The runner answers `POST /ui-bridge/control/page-health` with an **Observation
envelope**, always HTTP 200. `status` is the whole answer, and the report exists
only under one of its states:

| `status` | Meaning | What to report |
|---|---|---|
| `measured` | The runner saw the page's elements; `value` is the report below | The findings, qualified by `provenance.coverage` |
| `unknown` | The runner **could not look**; `unknown.code` says why | "Page health UNKNOWN: `<code>`" — **never** healthy, **never** broken |

An `unknown` carries no report and no severity at all — it used to be computed
anyway, over zero elements, and read as a CRITICAL empty page. Before this change
"the bridge returned nothing" and "the page is blank" were one answer; they are
now two, and only `measured` is a statement about the page.

### `unknown.code` → the one next action

| `unknown.code` | Cause | Next action |
|---|---|---|
| `producer_failed` | The `discover` IPC to the page failed (bridge down, timed out) | Check the runner's frontend is up: `curl -s http://127.0.0.1:9876/health`; retry once it answers |
| `input_missing` | `discover` answered with no `elements` array (`observedAt: null` — no sample), OR visible elements exist and none carries a `normalizedRect` (nothing for the grid to place) | Capture `discover` directly and report what it returned; for the geometry case, the page's elements carry no layout rects — do not assess the layout |
| `producer_not_run` | `discover` returned zero elements — nothing is registered yet | The page is not hydrated or not instrumented: wait for load / navigate, then re-run. This is NOT an empty page |
| any other code | See the contract in `qontinui-schemas/rust-vision-core/src/observation.rs` | Report the code verbatim as UNKNOWN |

### Coverage — elements the grid could not place

`provenance.coverage.considered` is the number of visible elements,
`coverage.measured` those carrying a `normalizedRect`, and
`coverage.unmeasured` names the rest as `{dimension: "geometry", count, code:
"input_missing"}`. A low `spatial_coverage` finding beside a non-zero unmeasured
`geometry` count is **not** evidence of an empty content area — those elements
exist and the grid could not place them. Say so in the report.

## How To Use

### From curl (any consumer)

```bash
# Runner — answers the Observation envelope described above
curl -s -X POST http://127.0.0.1:9876/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}' \
  | jq '.data | if .status == "measured" then {status, summary: .value.summary, coverage: .provenance.coverage}
                else {status, code: .unknown.code, detail: .unknown.detail} end'

# Web frontend (served by the @qontinui/ui-bridge SDK build it pins — see the note below)
curl -s -X POST http://127.0.0.1:3001/api/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}'

# A paired device (phone / app) — hit ITS OWN endpoint directly
curl -s -X POST http://<device-ip>:8087/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}'
```

> **Which shape you get depends on who answers.** The runner's `:9876` route is
> the runner's own implementation and answers the envelope. The web frontend and
> device endpoints are served by the UI Bridge SDK build each app pins; a build
> that predates the SDK's envelope still answers the bare report
> (`{summary, findings, ...}` with no `status`). If a reply carries no `status`
> key, it is that older shape: treat an empty `findings` over zero elements as
> UNKNOWN, not as a clean or broken page.

> **Targeting a device:** page-health is element-data-only (it calls
> `discover` internally; no screenshot, no frame pipeline). Unlike the
> pixel skills (`/visual-check`, `/visual-audit`), it takes **no `target`
> field** — to assess a device's page, POST directly to that device's own
> `control/page-health` endpoint (each UI Bridge server, including the
> phone's native server on `:8087`, exposes it). The runner is not in the
> loop, so there is nothing to thread a `target` through.

## Response Format

A measured answer:

```json
{
  "success": true,
  "data": {
    "status": "measured",
    "value": {
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
    },
    "provenance": {
      "producer": { "id": "runner/page-health", "version": "<runner version>" },
      "observedAt": "2026-09-30T12:00:00Z",
      "evaluatedAt": "2026-09-30T12:00:00.004Z",
      "coverage": { "considered": 38, "measured": 38, "unmeasured": [] },
      "confidence": null,
      "cache": null,
      "source": { "transport": "runner-ipc",
                  "discover": { "options": { "includeHidden": true, "interactiveOnly": false } } }
    }
  }
}
```

An unknown answer (the page registered nothing yet):

```json
{
  "success": true,
  "data": {
    "status": "unknown",
    "unknown": { "code": "producer_not_run",
                 "detail": "the page has registered no elements yet (not hydrated, or not instrumented), so there was nothing to assess" },
    "provenance": {
      "producer": { "id": "runner/page-health", "version": "<runner version>" },
      "observedAt": "2026-09-30T12:00:00Z", "evaluatedAt": "2026-09-30T12:00:00.001Z",
      "coverage": { "considered": 0, "measured": 0, "unmeasured": [] },
      "confidence": null, "cache": null,
      "source": { "transport": "runner-ipc", "discover": { "...": "..." } }
    }
  }
}
```

`provenance.observedAt` is when the `discover` reply arrived (the sample time);
`confidence: null` states the report is a deduction over element data, not an
estimate. `summary` is the worst finding severity **inside a measured report**
— never read it off an unknown, which carries none.

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

The endpoint calls discover internally, judges whether it could see the page
at all (the `unknown` codes above), then analyzes the element data server-side:

1. Builds a 20x20 viewport coverage grid from element `normalizedRect` positions
2. Classifies elements into layout regions (sidebar/header/content) by center position
3. Scans `textContent` and CSS classes for error/loading/empty signals (filtering out nav elements to avoid false positives)
4. Checks interactive element states (enabled, pointer-events)
5. Returns structured findings with severity levels and an ASCII heatmap

No screenshots, no browser, no visual model — just the structured element data the SDK already provides.
