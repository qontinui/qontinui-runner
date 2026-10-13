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

Every page-health producer — the runner's route and the `@qontinui/ui-bridge`
SDK's (≥ 0.30.0) — answers `POST /ui-bridge/control/page-health` with an
**Observation envelope**, always HTTP 200. `status` is the whole answer, and the
report exists only under one of its states:

| `status` | Meaning | What to report |
|---|---|---|
| `measured` | The producer saw the page's elements; `value` is the report below | The findings, qualified by `provenance.coverage` |
| `unknown` | The producer **could not look**; `unknown.code` says why | "Page health UNKNOWN: `<code>`" — **never** healthy, **never** broken |
| `absent` | Not a page-health answer today (no producer emits it) | Report it verbatim with the producer id; do not read it as an empty page |

An `unknown` carries no report and no severity at all — it used to be computed
anyway, over zero elements, and read as a CRITICAL empty page. Before this change
"the bridge returned nothing" and "the page is blank" were one answer; they are
now two, and only `measured` is a statement about the page.

**Name the producer from the reply, never from the URL.** `provenance.producer.id`
says who analyzed the page — `runner/page-health` (the runner's own analyzer),
`sdk/page-health` (the `@qontinui/ui-bridge` SDK's, in-page or through the web
relay), `sdk-server/page-health` (`@qontinui/ui-bridge-server`) or
`sdk-native/page-health` (`@qontinui/ui-bridge-native`, a device). Which one
answers a given URL is a property of the build serving it and is expected to
move (the runner is slated to forward the SDK's answer), so quote the field.

**`sdk-server/page-health` is a different analyzer with a different `value`.**
Its report is `{status: "healthy"|"degraded"|"unhealthy", findings: [{key,
severity: "critical"|"warning"|"info", message, details?}], heatmap, stats}` —
no `summary`, no `check`. That inner `value.status` is the report's roll-up, not
the envelope `status`. The check table, severities and single-column rule below
describe the other three producers (`runner/`, `sdk/`, `sdk-native/`).

### `unknown.code` → the one next action

The codes are a closed set (`UnknownCode` in
`qontinui-schemas/rust-vision-core/src/observation.rs`, mirrored by
`@qontinui/ui-bridge` `observation.ts`). Branch on the code, never on
`unknown.detail` (prose for humans).

| `unknown.code` | Cause | Next action |
|---|---|---|
| `producer_failed` | The producer ran and errored: the runner's `discover` IPC failed (bridge down, timed out), or the SDK's registry snapshot / analysis threw | Check the answering app is up (`curl -s http://127.0.0.1:9876/health` for the runner); retry once it answers |
| `input_missing` | The snapshot / `discover` reply carried no `elements` array (`observedAt: null` — no sample), OR visible elements exist and none carries a `normalizedRect` (nothing for the grid to place), OR the web relay refused a malformed snapshot | Capture `discover` directly and report what it returned; for the geometry case, the page's elements carry no layout rects — do not assess the layout |
| `producer_not_run` | Zero elements / zero registered components — nothing is registered yet | The page is not hydrated or not instrumented: wait for load / navigate, then re-run. This is NOT an empty page |
| `app_unreachable` | The web relay could not fetch a snapshot from the browser tab and holds none cached | Open or register a tab on that page (`ui-bridge-inject`), then re-run |
| `stale_input` | The web relay's refresh failed and only an older cached snapshot exists (`provenance.observedAt` is that snapshot's time) | Do not report the cached page as current; re-run once the tab answers |
| `capability_unavailable_in_build`, `below_confidence_floor`, `model_reply_unparseable`, `needs_multi_frame_input` | Not emitted by any page-health producer today | Report the code verbatim as UNKNOWN and record a finding — a page-health reply carrying one is a contract break |

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
# Read `status` FIRST, on every producer. Save the filter once:
PH_JQ='. as $r | $r.data | if .status == "measured" then {status, producer: .provenance.producer.id, summary: (.value.summary // .value.status), coverage: .provenance.coverage}
  elif .status == "unknown" then {status: "UNKNOWN", producer: .provenance.producer.id, code: .unknown.code, detail: .unknown.detail}
  elif .status == "absent" then {status, producer: .provenance.producer.id}
  else {status: "UNRECOGNISED (no status key: an error reply or a pre-envelope build)", raw: $r} end'  # envelope-ok: branches on .data.status first; a reply with no status renders UNRECOGNISED, never a default

# Runner
curl -s -X POST http://127.0.0.1:9876/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}' | jq "$PH_JQ"

# Web frontend (served by the @qontinui/ui-bridge SDK build it pins — see the note below)
curl -s -X POST http://127.0.0.1:3001/api/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}' | jq "$PH_JQ"

# A paired device (phone / app) — hit ITS OWN endpoint directly
curl -s -X POST http://<device-ip>:8087/ui-bridge/control/page-health -H "Content-Type: application/json" -d '{}' | jq "$PH_JQ"
```

> **Which shape you get depends on the build that answers.** The runner's
> `:9876` route and an SDK build ≥ `@qontinui/ui-bridge` 0.30.0 answer the
> envelope. A web or device endpoint serving an older SDK pin still answers the
> bare report (`{summary, findings, ...}` with no `status`) — the filter above
> labels it UNRECOGNISED. Over that older shape, an empty `findings` over zero
> elements is UNKNOWN, not a clean or broken page.

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
