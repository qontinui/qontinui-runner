---
name: visual-check
description: Get a text-only summary of what's on screen in a UI Bridge-connected app. Combines OCR (visible text + bbox) with a VLM caption (concise prose description). Returns no pixels — text only. Faster + cheaper than capture+Read for the 90% of debugging where text content + layout is enough.
user-invocable: true
---

# Visual Check

Pulls a **text-only** summary of what's on screen — no pixels in the response.
Wraps `POST /ui-bridge/vision/extract` (OCR) and `POST /ui-bridge/vision/describe`
(VLM caption) and merges them into one compact view.

This is Phase 4 of the UI Bridge Vision Pipeline plan. It exists because
"capture a PNG + Read it" is the wrong default for most debugging — the
runner usually already has the text content; you just need a way to ask
"what's there?" without a model round-trip on pixels.

## When To Use

- **"Did the action work?"** — call after a click/type to verify the new
  text state, without sending a screenshot to your own model.
- **"What does this element look like?"** — pass an `elementId` and get
  the text + layout describing just that subregion.
- **"What's on screen right now?"** — no args, get a full-frame summary.
- **"Find element matching X"** — search the returned `aggregateText` /
  blocks for the string instead of fan-out scanning.

**Don't use** for pixel-perfect comparison (use `vision/diff`) or for
producing image bytes a downstream model needs (use `vision/capture`).
**Don't use** when the agent's own vision model can read the screenshot
directly and a pixel-aware answer matters (e.g., color, exact
typography); call `vision/capture` + send to the agent's vision model.

## How To Use

### Quick full-frame summary

```bash
# Runner (primary or temp)
curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/extract \
  -H "Content-Type: application/json" \
  -d '{}' | head -c 1000

curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/describe \
  -H "Content-Type: application/json" \
  -d '{"maxTokens": 256}'
```

### Element-scoped

```bash
# Just the terminal panel
curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/extract \
  -H "Content-Type: application/json" \
  -d '{"element":"button-terminal-active"}'
```

### Region-scoped

```bash
# A pixel-space rect (e.g., the top status bar)
curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/extract \
  -H "Content-Type: application/json" \
  -d '{"region":{"x":0,"y":0,"w":1280,"h":40}}'
```

### Targeted question via describe

```bash
curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/describe \
  -H "Content-Type: application/json" \
  -d '{"prompt":"Is the Save button enabled? If so, where is it?", "maxTokens": 128}'
```

### Targeting a remote device (phone / registered app)

By default these endpoints capture the **runner's own desktop window**. To
analyze a *different* surface — a paired phone, an HTTP-registered app, an
adb device — pass a `target` field with the device/app id. The runner sources
the frame from that target instead (via its `control/screenshot` endpoint, or
adb framebuffer for a bare serial) and runs OCR/VLM on **those** pixels:

```bash
# OCR the text on a paired phone, not the runner desktop
curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/extract \
  -H "Content-Type: application/json" \
  -d '{"target":"<device-id>"}'

# VLM-caption the phone screen
curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/describe \
  -H "Content-Type: application/json" \
  -d '{"target":"<device-id>","maxTokens":256}'
```

`target` resolves, in order, against: a registered physical device (its proxy
url), a registered app (its base url), then an adb serial / `emulator-NNNN`.
An unknown id is a **malformed request**, exactly like an unknown `element`:
HTTP **404** with `unknown vision target '<id>'` in `error` — the same on every
vision route. If the target resolves but its capture fails (no
`screenshotProvider` wired, the device is unreachable), the capture was
attempted and failed: `status: "unknown"`, `unknown.code: "producer_failed"`,
with `unknown.detail` naming the capture — never a silent fallback to the
runner desktop. So a `measured` answer always reflects the intended surface,
and `provenance.source.target` names it. `target` is part of the cache key, so device frames never collide with
desktop frames. Omit `target` for the default runner-desktop behavior.

## Response Shapes — the Observation envelope

Both routes answer **HTTP 200 with an Observation**, whatever happened. Read
`status` FIRST — it is the whole answer, and `value` exists only under one of
its three states:

| `status` | Meaning | What to report |
|---|---|---|
| `measured` | The model looked and saw something; `value` holds it | The text / caption, with its age (below) |
| `absent` | The model looked, with **full coverage**, and saw no text (it returned no blocks, or only whitespace-only / duplicate ones) | "No text in this region" — a real statement about the page |
| `unknown` | The producer **could not look**; `unknown.code` says why | "Could not read the screen: `<code>`" — **never** "the page is empty", **never** "the page is broken" |

An `unknown` is not a clean page and not a broken page. It says nothing about
the page at all. Reporting it as either is the defect this envelope exists to
prevent.

### `/vision/extract`

```json
{
  "success": true,
  "data": {
    "status": "measured",
    "value": {
      "blocks": [
        { "bbox": {"x": 120, "y": 44, "w": 88, "h": 32}, "text": "Save", "confidence": 0.97 },
        { "bbox": {"x": 220, "y": 44, "w": 88, "h": 32}, "text": "Cancel", "confidence": 0.95 }
      ],
      "aggregateText": "Save\nCancel",
      "dropped": { "belowConfidence": 1, "emptyText": 0, "deduplicated": 0 }
    },
    "provenance": {
      "producer": { "id": "runner/vision-extract", "version": "<runner version>" },
      "observedAt": "2026-09-30T10:00:00Z",
      "evaluatedAt": "2026-09-30T10:00:00.412Z",
      "coverage": { "considered": 3, "measured": 2,
                    "unmeasured": [{ "dimension": "text", "count": 1, "code": "below_confidence_floor" }] },
      "confidence": 0.95,
      "cache": { "hit": false, "storedAt": null, "keyInputs": ["mutation_id", "request"] },
      "source": { "kind": "window", "captureBackend": "MonitorCrop", "scaleFactor": 1.0,
                  "capturedAt": "2026-09-30T10:00:00Z", "width": 1280, "height": 800,
                  "target": null, "crop": null, "model": "paddleocr" }
    }
  }
}
```

`aggregateText` is the blocks joined newline-by-newline in scan order
(top-to-bottom, left-to-right). Use it for `contains` / regex searches.
`dropped` counts every raw model block post-processing removed, by reason. A
non-zero `dropped.belowConfidence` beside kept blocks is the DEGRADED case: the
answer stands, and `provenance.coverage.unmeasured` names the text the model saw
and was not sure of — a `contains` miss over it is not proof of absence.

The fixed rules: model returned no blocks — or only whitespace-only or duplicate
blocks, which post-processing drops (see `dropped`) — → `absent`; every block
carrying text scored under `minConfidence` → `unknown` / `below_confidence_floor`;
reply not parseable → `unknown` / `model_reply_unparseable`; endpoint unreachable
or non-2xx → `unknown` / `producer_failed`; the frame capture was attempted and
failed (window gone, device capture error) → `unknown` / `producer_failed`,
with `unknown.detail` saying it was the capture.

An unknown answer:

```json
{
  "success": true,
  "data": {
    "status": "unknown",
    "unknown": { "code": "producer_failed", "detail": "OCR call to model `paddleocr`: HTTP error calling vision endpoint: ..." },
    "provenance": { "producer": { "id": "runner/vision-extract", "version": "<runner version>" },
                    "observedAt": "2026-09-30T10:00:00Z", "evaluatedAt": "2026-09-30T10:00:00.050Z",
                    "coverage": { "considered": 0, "measured": 0, "unmeasured": [] },
                    "confidence": null, "cache": { "hit": false, "storedAt": null, "keyInputs": ["mutation_id", "request"] },
                    "source": { "...": "frame projection", "model": "paddleocr" } }
  }
}
```

### `/vision/describe`

`describe` is **dual-audience**: `value.description` is the prose caption and
`value.structured` is a closed-schema machine twin — itself an Observation, so
its two missing states stay apart:

```json
{
  "success": true,
  "data": {
    "status": "measured",
    "value": {
      "description": "A modal dialog asking the user to confirm deletion of \"workflow-3\". Two buttons: Save (disabled, gray) and Cancel (enabled, blue).",
      "structured": {
        "status": "measured",
        "value": {
          "elements": [
            { "role": "button", "text": "Save", "state": ["disabled"], "color": "gray",
              "bbox": {"x": 612, "y": 430, "w": 88, "h": 32} }
          ],
          "modals": [ { "kind": "confirm", "title": "Delete workflow-3?", "ctas": ["Save", "Cancel"] } ],
          "overlays": [],
          "layout": "centered",
          "confidence": 0.92
        },
        "provenance": { "...": "same shape as the outer provenance" }
      }
    },
    "provenance": {
      "producer": { "id": "runner/vision-describe", "version": "<runner version>" },
      "observedAt": "2026-09-30T10:00:00Z", "evaluatedAt": "2026-09-30T10:00:01.7Z",
      "coverage": { "considered": 1, "measured": 1, "unmeasured": [] },
      "confidence": 0.92,
      "cache": { "hit": true, "storedAt": "2026-09-30T10:00:01.8Z", "keyInputs": ["mutation_id", "request"] },
      "source": { "...": "frame projection", "model": "qontinui-grounding-v1",
                  "tokens": { "promptTokens": 1184, "completionTokens": 41, "totalTokens": 1225 } }
    }
  }
}
```

**Branch on `value.structured.status`, never regex-parse `description`:**

| `value.structured.status` | Meaning |
|---|---|
| `measured` | A twin that passed strict validation — branch on `value.structured.value` |
| `absent` | The VLM answered prose-only (offered no twin) — read `description` |
| `unknown` (`model_reply_unparseable`) | A twin was offered and FAILED strict validation — `description` still stands, and the outer `coverage.unmeasured` names `structured` |

The closed twin schema:

| Field | Shape | Notes |
|---|---|---|
| `elements[]` | `{ role: string, text?: string, state?: ("disabled"\|"loading"\|"selected"\|"focused")[], color?: string, bbox?: {x,y,w,h} }` | `role` is open free-text; `state` values are a **closed set** |
| `modals[]` | `{ kind: "confirm"\|"alert"\|"form", title?: string, ctas?: string[] }` | `ctas` = call-to-action button labels in reading order |
| `overlays[]` | `{ kind: "tooltip"\|"dropdown"\|"menu", text?: string }` | transient overlays |
| `layout` | `"centered"\|"split"\|"list"\|"grid"\|"custom"` | required |
| `confidence` | `number` (0–1) | required |

An empty caption, an unparseable reply, a failed endpoint or a missing frame make
the OUTER observation `unknown` with the matching code.

### `unknown.code` → the one next action

| `unknown.code` | Cause | Next action |
|---|---|---|
| `producer_failed` | The producer OR its capture failed — `unknown.detail` says which: *"frame capture failed …"* (window gone, device served no screenshot) or *"OCR/VLM call to model …"* (endpoint unreachable, timed out, non-2xx) | Capture: confirm the runner window / device (`GET http://127.0.0.1:9876/health`). Model: check `QONTINUI_VISION_*_ENDPOINT` (llama-swap `http://127.0.0.1:8100`); fall back to `discover` for text |
| `model_reply_unparseable` | The model answered with something that is not the requested JSON | Retry with `"force": true`; if it persists the model alias is wrong for the task |
| `below_confidence_floor` | The model saw text and scored all of it under `minConfidence` | Retry with a lower `minConfidence`, or a tighter `region` / `element` |
| any other code | See the contract table in `qontinui-schemas/rust-vision-core/src/observation.rs` | Report the code verbatim; do not infer a page state |

## Cache Behavior — quote the age

Both endpoints are cache-keyed on `provenance.cache.keyInputs` —
`["mutation_id", "request"]`. The mutation counter bumps on every
`control/click`, `control/type`, `control/navigate`, `control/scroll-page`, and
any frontend `__UI_BRIDGE__.mutationOccurred()` signal. **It does NOT see a page
that changed on its own** (async load, poll, timer). So when
`provenance.cache.hit` is `true`, **always quote `provenance.observedAt` (when
the pixels were sampled) and `provenance.cache.storedAt`** in your report: the
answer is about the page as it was then, and may be stale. Only `measured` and
`absent` answers are cached; an `unknown` is never served from cache.

Force a fresh call by passing `"force": true`.

## Skill Recipe

For most "what does this look like" questions, run both in sequence:

```bash
# 1. Get OCR blocks (fast, cheap, exact text)
EXTRACT=$(curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/extract \
  -H "Content-Type: application/json" -d '{}')

# 2. Read the status FIRST. An unknown is "could not look", never "empty".
case "$(echo "$EXTRACT" | jq -r '.data.status')" in
  measured) echo "TEXT BLOCKS:"; echo "$EXTRACT" | jq -r '.data.value.aggregateText' ;;
  absent)   echo "NO TEXT (the model looked with full coverage)" ;;
  unknown)  echo "COULD NOT READ: $(echo "$EXTRACT" | jq -r '.data.unknown.code') — $(echo "$EXTRACT" | jq -r '.data.unknown.detail')" ;;
  *)        echo "UNRECOGNISED REPLY (no status): $EXTRACT" ;;
esac
# Age: quote it whenever the answer came from the cache.
echo "$EXTRACT" | jq -r 'if .data.provenance.cache.hit then "CACHED: observedAt=\(.data.provenance.observedAt) storedAt=\(.data.provenance.cache.storedAt)" else empty end'

# 3. If the text alone is insufficient, get a VLM caption (slower)
DESCRIBE=$(curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/describe \
  -H "Content-Type: application/json" -d '{"maxTokens": 256}')
case "$(echo "$DESCRIBE" | jq -r '.data.status')" in
  measured)
    if [ "$(echo "$DESCRIBE" | jq -r '.data.value.structured.status')" = measured ]; then
      echo "STRUCTURED:"; echo "$DESCRIBE" | jq '.data.value.structured.value'
    else
      echo "CAPTION:"; echo "$DESCRIBE" | jq -r '.data.value.description'
    fi ;;
  unknown) echo "COULD NOT DESCRIBE: $(echo "$DESCRIBE" | jq -r '.data.unknown.code')" ;;
  *)       echo "UNRECOGNISED REPLY (no status): $DESCRIBE" ;;
esac
```

The OCR alone usually answers "is the right text on screen?" The VLM
`structured` twin disambiguates "what is this UI?" — elements, modals,
overlays, layout — as machine-readable data.

## Configuration

The endpoints route to llama-swap by default. Override per-process via:

| Env var | Default | Purpose |
|---|---|---|
| `QONTINUI_VISION_OCR_ENDPOINT` | `http://127.0.0.1:8100` | OCR HTTP base |
| `QONTINUI_VISION_OCR_MODEL` | `paddleocr` | OCR model alias |
| `QONTINUI_VISION_VLM_ENDPOINT` | `http://127.0.0.1:8100` | VLM HTTP base |
| `QONTINUI_VISION_VLM_MODEL` | `qontinui-grounding-v1` | VLM model alias |

Both default to llama-swap's local port (matches WSV's
`QONTINUI_WORLD_STATE_VERIFIER_ENDPOINT`). Point them at a remote
llama-swap or a different multiplexer by overriding before runner
startup.

If the model isn't reachable, the endpoint answers `status: "unknown"` with
`unknown.code: "producer_failed"` and the underlying error in `unknown.detail`
(HTTP 200 — a non-2xx now means only a malformed request, such as a region
outside the frame or an element id that does not resolve). Callers should fall
back to `vision/capture` + their own model, or to `discover` for a pixel-free
structural snapshot.

## Why "no pixels in the response"?

Two reasons:

1. **Tokens.** A 1568-px screenshot is ~150 KB. The aggregate text is
   usually ≤ 1 KB. Sending only text scales the conversation 100x
   further.
2. **Cost.** Vision-input pricing dominates the model bill. If a text
   answer is sufficient, paying for vision input is wasteful. Phase 4
   formalizes this preference at the architecture level.

The pixel-aware endpoints (`vision/capture`, `vision/annotate`,
`vision/diff`, `vision/raw`) remain available for the 10% of cases
where pixels are load-bearing.
