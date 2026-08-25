---
name: visual-audit
description: Run a declarative visual audit on a UI Bridge-connected page. Combines five vision-core analyzers (layout, typography, color, dynamic, elements) with the 10-assertion DSL (no_overlap, contains_text, text_fits_container, aligned_*, color_within, typography_consistent, no_layout_shift_since, no_clipping, animation_settled, contrast_meets_wcag). Returns structured findings + pass/fail per assertion — never pixels. Use as the canonical "audit this page for visual regressions / a11y violations / layout bugs" entrypoint.
user-invocable: true
---

# Visual Audit

Phase 6 of the UI Bridge Vision Pipeline. The runner exposes four endpoints
that turn declarative visual questions into structured answers:

| Endpoint | What it does |
|---|---|
| `POST /ui-bridge/vision/analyze` | Run one of the five analyzers (layout/typography/color/dynamic/elements). Returns `findings: [{kind, severity, region?, detail, elements?}]`. |
| `POST /ui-bridge/vision/assert` | Evaluate a list of declarative assertions over the captured frame + caller-supplied snapshot. Returns per-assertion pass/fail + reason. |
| `POST /ui-bridge/vision/baseline` | Capture a baseline image + register the snapshot's element bboxes under `name`. |
| `GET  /ui-bridge/vision/baselines` | List registered baselines. |

Both `analyze` and `assert` require a **caller-supplied ElementSnapshot**.
Get it from the runner's own `discover` endpoint, then pass it through:

```bash
# 1. Snapshot (caller supplies)
SNAP=$(curl -s http://localhost:9876/ui-bridge/control/discover | jq '.data')

# 2. Run the layout analyzer
curl -s -X POST http://localhost:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson snapshot "$SNAP" '{analyzer:"layout", snapshot:$snapshot}')" \
  | jq '.data.findings[]'
```

### Targeting a remote device (phone / registered app)

By default `analyze` / `assert` / `baseline` capture the **runner's own desktop
window**. To audit a *different* surface, pass a `target` field with the
device/app id — the runner sources the frame from that target. **Crucially, the
`snapshot` must also come from the same target**, because the assertion logic
matches element bboxes against the captured frame; a runner-sourced snapshot
over a device-sourced frame would be incoherent. Fetch `discover` from the
device, not the runner:

```bash
# Snapshot AND frame from the same target (the device serves both)
SNAP=$(curl -s "http://<device-ip>:8087/ui-bridge/control/discover" | jq '.data')

curl -s -X POST http://localhost:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"layout", snapshot:$s, target:"<device-id>"}')" \
  | jq '.data.findings[]'

# assert + baseline take `target` the same way
curl -s -X POST http://localhost:9876/ui-bridge/vision/assert \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{snapshot:$s, target:"<device-id>", assertions:[{"type":"no_clipping"}]}')"
```

`target` resolves against a registered physical device → registered app → adb
serial, in that order; an unknown id 500s with `unknown vision target '<id>'`,
and a target serving no screenshot fails loudly rather than silently capturing
the runner desktop. Omit `target` for the default runner-desktop behavior.

## The Five Analyzers

| Analyzer | What it checks |
|---|---|
| **layout** | Pairwise overlap among interactive elements (ignoring nested layouts), zero-area elements, alignment jitter on near-y-baseline groups (3+ elements within 3px). |
| **typography** | Font-family + font-size cluster counts. Flags >3 distinct families or >8 distinct sizes (warning + info — usual sign of design-system drift). |
| **color** | WCAG contrast for every text element with both `fg_color` and `bg_color` populated. Flags individual sub-AA (4.5:1) elements + a "contrast density" warning when ≥25% of text fails. |
| **dynamic** | Two-frame pixel diff. Currently requires the caller to wire up a `prior_frame` source; this maps to `vision/diff` for now. |
| **elements** | Empty-snapshot detection, no-interactive / no-text smell, sub-24×24 interactive targets (WCAG 2.5.8). |

## The Assertion DSL

Each assertion is a tagged-JSON object:

| Assertion | Body shape |
|---|---|
| `no_overlap` | `{"type":"no_overlap","elements":["btn-a","btn-b"],"tolerance_px":0}` |
| `contains_text` | `{"type":"contains_text","target":{"element":"h1"},"text":"Hello","kind":"contains"}` |
| `text_fits_container` | `{"type":"text_fits_container","element":"label-7"}` |
| `aligned_horizontally` | `{"type":"aligned_horizontally","elements":["a","b","c"],"axis_tolerance_px":2}` |
| `aligned_vertically` | `{"type":"aligned_vertically","elements":["nav-1","nav-2"]}` |
| `color_within` | `{"type":"color_within","element":"logo","expected":{"r":255,"g":51,"b":51},"delta_e_max":5}` |
| `typography_consistent` | `{"type":"typography_consistent","elements":["h1","h2"],"dimensions":["font_family"]}` |
| `no_layout_shift_since` | `{"type":"no_layout_shift_since","baseline":"v1.0-home","tolerance_px":2}` |
| `no_clipping` | `{"type":"no_clipping","region":{"x":0,"y":0,"w":1280,"h":720}}` |
| `animation_settled` | `{"type":"animation_settled","region":null,"settle_frames":3}` |
| `contrast_meets_wcag` | `{"type":"contrast_meets_wcag","element":"btn-save","level":"aa"}` |

### Example: audit the page for the kinds of bugs Phase 6 was designed to catch

```bash
SNAP=$(curl -s http://localhost:9876/ui-bridge/control/discover | jq '.data')

curl -s -X POST http://localhost:9876/ui-bridge/vision/assert \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson snapshot "$SNAP" '{
    snapshot: $snapshot,
    assertions: [
      { "type": "no_overlap", "elements": ["button-terminal-1", "button-terminal-2"] },
      { "type": "no_clipping" },
      { "type": "aligned_horizontally", "elements": ["nav-link-1","nav-link-2","nav-link-3"], "axis_tolerance_px": 2 }
    ]
  }')" | jq '.data'
```

Response:

```json
{
  "results": [
    { "passed": false, "detail": "button-terminal-1 and button-terminal-2 overlap by 1632 px²",
      "assertion": { "type": "no_overlap", "elements": ["button-terminal-1","button-terminal-2"] } },
    { "passed": true, "assertion": { "type": "no_clipping" } },
    { "passed": true, "assertion": { "type": "aligned_horizontally", "elements": [...] } }
  ],
  "allPassed": false
}
```

## Baselines

Register a "what good looks like" snapshot for a route:

```bash
curl -s -X POST http://localhost:9876/ui-bridge/vision/baseline \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson snapshot "$SNAP" '{
    name: "home-v1.2",
    snapshot: $snapshot
  }')"
```

Later, assert nothing has shifted:

```json
{ "type": "no_layout_shift_since", "baseline": "home-v1.2", "tolerance_px": 2 }
```

Baselines live in-process; they do **not** persist across runner
restarts. Use them for in-session regression checks, not durable
artifacts. List with `GET /ui-bridge/vision/baselines`.

## Canonical Audit Recipe (the original motivating use case)

After landing UI changes, run the full audit:

```bash
SNAP=$(curl -s http://localhost:9876/ui-bridge/control/discover | jq '.data')

# 1. Layout analyzer — catches the overlap bugs the plan was designed for
curl -s -X POST http://localhost:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"layout",snapshot:$s}')" \
  | jq '.data.findings[] | select(.severity != "info")'

# 2. Elements analyzer — sub-24×24 targets + empty-snapshot smells
curl -s -X POST http://localhost:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"elements",snapshot:$s}')" \
  | jq '.data.findings[]'

# 3. Contrast — only meaningful when snapshot supplies fg_color + bg_color
curl -s -X POST http://localhost:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"color",snapshot:$s}')" \
  | jq '.data.findings[]'

# 4. A default assertion bundle for every interactive page
curl -s -X POST http://localhost:9876/ui-bridge/vision/assert \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{
        snapshot: $s,
        assertions: [
          { "type": "no_clipping" }
        ]
      }')" | jq '.data.allPassed'
```

The terminal-tab overlap bug that triggered the whole Phase 6 design
becomes a one-line `no_overlap` invocation that reports it.

## When NOT to use

- **Pixel-level comparison vs a baseline image** — use `vision/diff` instead.
- **Generating image output for a downstream model** — use `vision/capture`.
- **OCR-only / "what text is visible"** — use `vision/extract` (the `/visual-check` skill).
- **Single-query "describe what's on screen"** — use `vision/describe`.

The audit endpoints are for declarative assertions: "I expect X to be
true; tell me whether it is." That's the right shape for regression
checks and a11y gates, not for ad-hoc exploration.

## Configuration

No env-var configuration — the endpoints route through the same
in-process state (cache, semaphore, baselines map) as the rest of the
vision pipeline. The OCR + VLM client envs (`QONTINUI_VISION_OCR_*`,
`QONTINUI_VISION_VLM_*` from Phase 4) apply if a `contains_text`
assertion needs to fall back to OCR.
