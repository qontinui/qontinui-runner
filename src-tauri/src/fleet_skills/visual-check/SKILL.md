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
An unknown id returns a 500 with `unknown vision target '<id>'`. If the target
is reachable but serves no screenshot (no `screenshotProvider` wired), the call
fails loudly with a "no `screenshot` field" error rather than silently falling
back to the runner desktop — so a green result always reflects the intended
surface. `target` is part of the cache key, so device frames never collide with
desktop frames. Omit `target` for the default runner-desktop behavior.

## Response Shapes

### `/vision/extract`

```json
{
  "success": true,
  "data": {
    "blocks": [
      { "bbox": {"x": 120, "y": 44, "w": 88, "h": 32},
        "text": "Save", "confidence": 0.97 },
      { "bbox": {"x": 220, "y": 44, "w": 88, "h": 32},
        "text": "Cancel", "confidence": 0.95 }
    ],
    "aggregateText": "Save\nCancel",
    "model": "paddleocr",
    "cached": false
  }
}
```

`aggregateText` is the blocks joined newline-by-newline in scan order
(top-to-bottom, left-to-right). Use it for `contains` / regex searches.

### `/vision/describe`

`describe` is **dual-audience** (UI Bridge diagnostic-discipline plan §8
Phase 4). It returns the prose caption *and* a closed-schema machine twin:

```json
{
  "success": true,
  "data": {
    "description": "A modal dialog asking the user to confirm deletion of \"workflow-3\". Two buttons: Save (disabled, gray) and Cancel (enabled, blue) in the bottom-right.",
    "structured": {
      "elements": [
        { "role": "button", "text": "Save", "state": ["disabled"],
          "color": "gray", "bbox": {"x": 612, "y": 430, "w": 88, "h": 32} },
        { "role": "button", "text": "Cancel", "color": "blue",
          "bbox": {"x": 712, "y": 430, "w": 88, "h": 32} }
      ],
      "modals": [
        { "kind": "confirm", "title": "Delete workflow-3?",
          "ctas": ["Save", "Cancel"] }
      ],
      "overlays": [],
      "layout": "centered",
      "confidence": 0.92
    },
    "tokens": { "promptTokens": 1184, "completionTokens": 41, "totalTokens": 1225 },
    "model": "qontinui-grounding-v1",
    "cached": true
  }
}
```

**`structured` is canonical; `description` is the human sibling.** Branch
on `structured` — never regex-parse `description`. The closed schema:

| Field | Shape | Notes |
|---|---|---|
| `elements[]` | `{ role: string, text?: string, state?: ("disabled"\|"loading"\|"selected"\|"focused")[], color?: string, bbox?: {x,y,w,h} }` | `role` is open free-text; `state` values are a **closed set** |
| `modals[]` | `{ kind: "confirm"\|"alert"\|"form", title?: string, ctas?: string[] }` | `ctas` = call-to-action button labels in reading order |
| `overlays[]` | `{ kind: "tooltip"\|"dropdown"\|"menu", text?: string }` | transient overlays |
| `layout` | `"centered"\|"split"\|"list"\|"grid"\|"custom"` | required |
| `confidence` | `number` (0–1) | required |

Arrays are always present (possibly `[]`). Optional object keys are
**omitted, not null** when absent.

**`structured` is `Option` — it can be absent.** When the VLM reply was
prose-only or failed strict schema validation, the response falls back to
**prose-only**: `description` is still populated (best-effort caption) and
the `structured` key is **omitted entirely**. The runner logs a
`UB-VLM-STRUCTURED-PARSE-FAIL` diagnostic on that path. So:

- `structured` present → branch on it (preferred).
- `structured` absent → degrade to reading `description` as before; this
  is the documented graceful-fallback contract, never an error (the
  endpoint does **not** 500 on a structured-parse failure).

## Cache Behavior

Both endpoints are cache-keyed on `(mutation_id, request shape, model,
…)`. The mutation counter bumps on every `control/click`, `control/type`,
`control/navigate`, `control/scroll-page`, and any frontend
`__UI_BRIDGE__.mutationOccurred()` signal. Cache hits return in <5ms;
misses pay the model latency (~300ms for OCR, ~1–2s for VLM cold path).

Force a fresh call by passing `"force": true`.

## Skill Recipe

For most "what does this look like" questions, run both in sequence:

```bash
# 1. Get OCR blocks (fast, cheap, exact text)
EXTRACT=$(curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/extract \
  -H "Content-Type: application/json" -d '{}')

# 2. If the text alone is insufficient, get a VLM caption (slower)
DESCRIBE=$(curl -s -X POST http://127.0.0.1:9876/ui-bridge/vision/describe \
  -H "Content-Type: application/json" -d '{"maxTokens": 256}')

# 3. Combine — text is usually enough; structured twin disambiguates
echo "TEXT BLOCKS:"; echo "$EXTRACT" | jq -r '.data.aggregateText'
echo
# Prefer the structured twin; fall back to prose only if it is absent.
echo "STRUCTURED:"; echo "$DESCRIBE" | jq -e '.data.structured' 2>/dev/null \
  || echo "$DESCRIBE" | jq -r '.data.description'
```

The OCR alone usually answers "is the right text on screen?" The VLM
`structured` twin disambiguates "what is this UI?" — elements, modals,
overlays, layout — as machine-readable data. `description` is the human
sibling; read it only when `structured` is absent (the documented
prose-only fallback).

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

If the model isn't reachable, the endpoint returns 500 with the
underlying error. Callers should fall back to `vision/capture` + their
own model, or to `discover` for a pixel-free structural snapshot.

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
