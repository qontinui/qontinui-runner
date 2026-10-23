---
name: visual-audit
description: Run a declarative visual audit on a UI Bridge-connected page. Combines six vision-core analyzers (layout, typography, color, dynamic, elements, image) with the 20-assertion DSL (no_overlap, element_above, contains_text, text_fits_container, aligned_*, color_within, typography_consistent, no_layout_shift_since, no_clipping, animation_settled, contrast_meets_wcag, and eight L1 image assertions). Returns structured findings, per-image values + a three-way outcome (passed/failed/unknown) per assertion — never pixels. Use as the canonical "audit this page for visual regressions / a11y violations / layout bugs" entrypoint. Its image arm operates a candidate image IN SITU — swapped into its real slot (the SDK media override, through ui-bridge-inject) across the capture matrix, hover, a throttled network and the loading-to-loaded timeline — then restores the page and proves it did.
user-invocable: true
---

# Visual Audit

Phase 6 of the UI Bridge Vision Pipeline. The runner exposes four endpoints
that turn declarative visual questions into structured answers:

| Endpoint | What it does |
|---|---|
| `POST /ui-bridge/vision/analyze` | Run one of the six analyzers (layout/typography/color/dynamic/elements/image). Returns `verdict` (READ THIS FIRST — see below), `analyzer` (echoed back), `findings: [{kind, severity, analyzer?, region?, detail, elements?, confidence}]`, `imageValues?` (the `image` analyzer's measurements — see below), and the provenance set: `coverage?`, `frame?`, `frameError?`, `evaluatedAt`, `snapshotAttribution`. |
| `POST /ui-bridge/vision/assert` | Evaluate a list of declarative assertions over the caller-supplied snapshot (plus OCR blocks and the baseline registry). **Only the L1 image assertions read the frame** (see the DSL table) — every other assertion evaluates from the snapshot, and for those the captured frame is reported as provenance but consulted by no verdict. Returns `results: [{passed, outcome, detail?, assertion, confidence}]` — `outcome` is three-way, not pass/fail — plus `allPassed` and the same provenance set. |
| `POST /ui-bridge/vision/baseline` | Capture a baseline image + register the snapshot's element bboxes under `name`. |
| `GET  /ui-bridge/vision/baselines` | List registered baselines. |

`/control/visibility`, discussed later, is **not** among them — it lives on the
web UI Bridge SDK, a different surface, and the runner 404s it.

Both `analyze` and `assert` require a **caller-supplied ElementSnapshot**.
> ### ⚠️ A raw `discover` payload is NOT a snapshot. Project it first.
>
> This skill used to document `curl .../discover | jq '.data'` piped straight
> into `analyze`. **That recipe silently returns a clean bill of health on a
> broken page**, and it is why the occlusion bug on the runner's Terminal page
> went unreported by every audit run against it.
>
> `DiscoveredElement` and vision-core's `Element` share exactly ONE field —
> `id`. Geometry lives at `state.rect` as `{x,y,width,height}` floats, not at
> `bbox` as `{x,y,w,h}` ints; text lives at `state.textContent`;
> `interactable` is derived, not carried. Rust's `Element` has no
> `deny_unknown_fields`, so the mismatched payload **parses** — into a
> snapshot where every element is `bbox: None, text: None,
> interactable: false`. The layout analyzer then filters its pairwise checks
> on exactly those fields, finds an empty set, and returns **zero findings**,
> byte-identical to a genuine pass.
>
> `rust-vision-core` pins this with a test named
> `a_raw_discover_payload_is_not_a_supported_input`. Project properly, or
> don't ask.
>
> **The server now refuses this, and that is the part you can rely on.** Every
> analyzer computes a `SnapshotCoverage` from the snapshot itself and returns an
> explicit `verdict`. A snapshot with no measurable geometry makes `layout`
> answer **`{"state":"blocked", "reason":…}`** — *the preconditions were not
> met*, which is a distinct answer from `"checked"` with an empty finding list,
> and it is not green. You no longer have to read a stderr line to find out
> whether a clean verdict meant anything: **read `verdict.state`.**

Get the snapshot from the runner's own `discover` endpoint and run it through
the projection script:

```bash
# 1. Discover -> project into a real ElementSnapshot.
#    --stats goes to stderr; READ IT (see below).
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/control/discover \
     -H 'Content-Type: application/json' -d '{"interactive_only":false}' \
  | python3 "$ROOT/qontinui-claude-config/scripts/uibridge-to-elementsnapshot.py" \
      --stats > /tmp/snapshot.json

# 2. Run the layout analyzer (occlusion, overlap, zero-area, alignment).
python3 -c 'import json,sys;print(json.dumps({"analyzer":"layout","snapshot":json.load(open("/tmp/snapshot.json"))}))' \
  | curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
      -H "Content-Type: application/json" -d @- \
  | python3 -c 'import json,sys;[print("[%s] %s: %s" % (f["severity"], f["kind"], f["detail"])) for f in json.load(sys.stdin)["data"]["findings"]]'
```

`python3`, not `jq` — jq is absent on the Windows operator box, and a missing
binary there reads identically to an empty result.

**Read the response's `verdict`. The server decides this now — you are not the
gate.** Every `analyze` response carries an explicit `verdict`, and — with one
exception — a `coverage` object: `elements`, `withGeometry`, `withStacking`,
`withText`, `interactable`. Those are the same MEASUREMENTS `--stats` reports
(minus `withStacking`, which the projection can never populate), but not the
same spellings — the script prints prose ("118 with geometry, 40 with text,
..."), so do not grep its output for these keys. Note also
that `coverage.elements` is the PROJECTED count (`snapshot.elements.len()`),
i.e. `--stats`'s numerator, not its `projected/input` pair. The
`coverage` is absent for either of two reasons, and they are different facts:
`dynamic` takes no snapshot and so emits none even when you supplied one, or
you supplied none at all. `snapshotAttribution.state` tells you which — `absent`
means you sent nothing, anything else means the analyzer declined to count what
it was given. The presence rule differs between the two routes — see the assert section
below.

| `verdict.state` | Means | Green? |
|---|---|---|
| `"checked"` | Preconditions met. An empty finding list here genuinely means clean. | yes |
| `"degraded"` | Ran, but a named dimension was unmeasurable — e.g. no stacking order, so occlusion is UNKNOWN. Findings are real but incomplete. Carries a `reason`. | yes |
| `"blocked"` | Preconditions **not** met. The input was too impoverished to check anything, so the finding list answers nothing. Carries a `reason`. | **no** |

The verdict is an internally-tagged object, so read `verdict.state` (and
`verdict.reason` on the two non-`checked` states), not a bare string.

A snapshot whose elements carry no geometry makes `layout` answer `Blocked`
rather than returning `[]`. That is the whole point: a `layout` result that
used to be byte-identical to a clean page now carries a verdict that is not
green. Note the scope — this is a statement about each analyzer's own
preconditions, not a guarantee that some other analyzer cannot still return an
honestly-empty `Checked`.

The `--stats` line still prints, and is still useful for seeing *why* a verdict
came out the way it did before you send anything — but it is **no longer the
defence**. It reports the same counters the server now computes for itself:

```
projected 118/118 elements: 118 with geometry, 40 with text, 78 interactive, 2 with a hit-tested occluder, 6 with media
```

> **`Degraded` is green on purpose, and this is worth understanding.** No
> snapshot from this projector carries stacking order: it never emits
> `z_index`, because a `discover` payload holds only the CSS `zIndex`, which is
> resolved per stacking context and inverts occlusion verdicts when compared
> across them (vision-core's producer contract forbids projecting it). So a
> `Degraded`-fails-the-gate rule would fire on every page and teach everyone to
> ignore it. `Degraded` is a statement about *coverage*, not a defect. Only
> `Blocked` is non-green. Occlusion direction then comes from the SDK's own
> hit-test, `occluded_by`, which the projection carries verbatim.
>
> **And `coverage.withStacking` counts POPULATED ranks, not correct ones.** The
> layout analyzer compares `z_index` across stacking contexts, while a CSS
> `zIndex` read is per-context; where some other producer emits the latter,
> occlusion verdicts can invert while coverage reads 100 %. A high
> `withStacking` is not a trust signal.

### No runner window? The checks still run.

`analyze` and `assert` capture a frame **best-effort**. Layout, typography and
elements are pure geometry over the snapshot, and only the L1 image assertions
read the frame, so a runner reporting `frontendState: "window_missing"` still
answers every geometric question. A failed capture comes back as
`frameError` in the response — check for it rather than assuming the pixels
agreed. Only `color`, `dynamic` and `image` need the frame, and without one
they answer `Blocked` with a `skipped` finding; the pixel-reading image
assertions answer `outcome: "unknown"` (a slot placeholder marker still fails
`no_placeholder_images`, since that needs no pixels, and `image_focal_visible`
reads media facts, not the frame).

The same holds for the `vision-audit` binary, where `--frame` is optional —
that is the path qontinui-web's CI style gate uses, with no runner in the loop
at all.

### Targeting a remote device (phone / registered app)

By default `analyze` / `assert` / `baseline` capture the **runner's own desktop
window**. To audit a *different* surface, pass a `target` field with the
device/app id — the runner sources the frame from that target. **Crucially, the
`snapshot` must also come from the same target**, because the assertion logic
matches element bboxes against the captured frame; a runner-sourced snapshot
over a device-sourced frame would be incoherent. Fetch `discover` from the
device, not the runner:

```bash
# Snapshot AND frame from the same target (the device serves both).
#
# ⚠️ DO NOT run this one through the projection script — this is the ONE place
# in this skill where `| jq '.data'` is correct. The React Native SDK on :8087
# emits the runner-native vision-core shape ALREADY: `bbox {x,y,w,h}` at top
# level, `fg_color` / `font_size_px`, and a truthful `interactable`. It is an
# ElementSnapshot on arrival, by design — see ui-bridge
# `packages/ui-bridge-native/src/core/vision-fields.ts`, whose header says the
# fields are emitted "so a visual-audit caller can post the snapshot through
# with no transform", and `rust-vision-core/src/element_snapshot.rs`, whose
# `bbox` doc-comment names a mobile `discover` snapshot as a supported source.
# The projector reads the WEB shape (`state.rect`, `state.computedStyles`), so
# projecting a native payload DROPS every one of those fields — measured: 0
# with geometry, and every element marked interactable by `inferActions`.
SNAP=$(curl -sS "http://<device-ip>:8087/ui-bridge/control/discover" | jq '.data')

# -sS, not -fsS: an unknown `target` 500s with `unknown vision target '<id>'`
# in the RESPONSE BODY, which the paragraph below tells you to read. `-f`
# discards the body and prints only curl's own exit-22 line, which cannot
# distinguish a typo'd id from an offline device.
curl -sS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"layout", snapshot:$s, target:"<device-id>"}')" \
  | jq '.data.findings[]'

# assert + baseline take `target` the same way
curl -sS -X POST http://127.0.0.1:9876/ui-bridge/vision/assert \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{snapshot:$s, target:"<device-id>", assertions:[{"type":"no_clipping"}]}')"
```

`target` resolves against a registered physical device → registered app → adb
serial, in that order; an unknown id 500s with `unknown vision target '<id>'`,
and a target serving no screenshot fails loudly rather than silently capturing
the runner desktop. Omit `target` for the default runner-desktop behavior.

## The Six Analyzers

| Analyzer | What it checks |
|---|---|
| **layout** | **Directed occlusion** (which element is on top and what it hides — see below), pairwise overlap among interactive elements (ignoring nested layouts), zero-area elements, alignment jitter on near-y-baseline groups (3+ elements within 3px). |
| **typography** | Font-family + font-size cluster counts. Flags >3 distinct families or >8 distinct sizes (warning + info — usual sign of design-system drift). |
| **color** | WCAG contrast for every text element with both `fg_color` and `bg_color` populated. Flags individual sub-AA (4.5:1) elements + a "contrast density" warning when ≥25% of text fails. |
| **dynamic** | Two-frame pixel diff. Currently requires the caller to wire up a `prior_frame` source; this maps to `vision/diff` for now. |
| **elements** | Empty-snapshot detection, no-interactive / no-text smell, sub-24×24 interactive targets (WCAG 2.5.8). |
| **image** | Per-image pixel VALUES, never a verdict. For every snapshot element carrying `media`, read from its crop of the frame: effective resolution and aspect distortion (object-fit aware), sharpness and compression blockiness, an OKLab palette with a photographic/flat class, busyness (also under text laid over it), pHash and dHash, placeholder signals, edge contrast against the surround, the natural-image region left visible, and saliency; for the page, colourfulness, clutter and white space, plus near-duplicate pairs. Needs the frame (`Blocked` without one); an image-free snapshot is `Degraded`, which is green. |

**`image` answers in `imageValues`, not in `findings`.** Its findings are
`info` only, so it can never trip a `--fail-on` threshold (the style gate runs
`vision-audit analyze --analyzer all --fail-on critical`). Each value is an
observation with its own provenance: a pixel value carries a `confidence` (the
share of the image's box that was inside the frame and not painted over), and
a value the producer did not measure is `unknown` with a code, never `0`. An
image reported `visible: false` with no `visibility_reason` is unreadable, not
left out. A check on those values gates only as an assertion with the caller's
own threshold — see the image rows of the DSL table.

### `occlusion` vs `overlap` — they answer different questions

`overlap` asks *"do two clickable targets collide?"*: interactive elements
only, and full nesting is exempted as intentional layout (a button inside its
container).

`occlusion` asks *"is anything hidden from the reader?"*, and inverts all three
of those choices on purpose:

| | `overlap` | `occlusion` |
|---|---|---|
| Participants | `interactable` only | every element with a bbox |
| Full containment | exempt (intentional nesting) | the WORST case, always reported |
| Direction | none — symmetric pair | the SDK's hit-test (`occluded_by`), or a resolved paint rank (`z_index`), names occluder → occluded |
| Severity | Warning | **Critical** when the covered element has text |

The participant rule is the one that matters most in practice: what a floating
widget hides is nearly always a *label* — a name, a status, a count — and
labels are never interactive, so `overlap` structurally cannot see this bug
class.

From this projection — which carries `occluded_by` and never `z_index` — a
finding reads:

```
[critical] occlusion: terminal-zone-header-8 is covered by terminal-zone-minimap
           (reported by the snapshot producer's hit-test)
```

**`occlusion_unknown` is not a pass.** When intersecting elements carry neither
a hit-test attribution nor a usable paint rank, the analyzer reports that it
could not determine a direction. Treat it as UNKNOWN, never as "nothing is
covered" — and do not "fix" it by feeding CSS `zIndex` in as `z_index`, which
inverts verdicts across stacking contexts.

Ancestor/descendant pairs are exempt — a child painting over its own parent's
box is what nesting *is*.

### `/control/visibility` — the focused question

`analyze` sweeps the page. When you want just "what is covering what", the
**web UI Bridge SDK** (`@qontinui/ui-bridge`) exposes a dedicated endpoint that
reports the directed relation — occluder → occluded — sourced from the
registry's own `elementFromPoint` hit-test (`state.occludedBy` /
`occludedPct`). The hit-test observes what the compositor actually painted, so
it sees `clip-path`, transformed ancestors and scroll clipping that a
bounding-box model cannot derive.

**Which surfaces serve it — check before you read a 404 as a pass:**

| Surface | `/control/visibility` |
|---|---|
| Web SDK in-page (`@qontinui/ui-bridge` server handlers) | **served** |
| qontinui-web relay (`:3001/api/ui-bridge`, `https://qontinui.io/api/ui-bridge`) | **served** — relay twin, hit-test half only, so findings are a SUBSET of the in-page endpoint's |
| Runner `:9876` (Tauri WebView) | **404 — not served.** The runner has its own **Rust** route table (`qontinui-runner/src-tauri/src/mcp/ui_bridge/routing.rs`); the frontend's `@qontinui/ui-bridge` npm dependency does not put routes on that port |
| React Native (`@qontinui/ui-bridge-native`) | **404 — not served**, pinned by `packages/ui-bridge-native/src/server/__tests__/http-status-mapping.test.ts` |

```bash
# Web SDK / relay surface — a DIFFERENT APPLICATION from the runner's :9876
# that every other example in this skill targets. Only run this when the page
# you are auditing is itself qontinui-web (see the applicability note below).
# -f so the 404 below is a non-zero exit rather than a 0 with an error body,
# -S so it still says why under -s. A 404 that exits 0 reads as a pass.
curl -fsS -X POST http://localhost:3001/api/ui-bridge/control/visibility \
  -H 'Content-Type: application/json' -d '{"minRatio":0.02}'
```

**Only ask this of the surface you are actually auditing.** The endpoint
answers about the surface it is sent to, and `:3001` is qontinui-web — not the
runner. This skill's own worked example is a *runner* page (the Terminal-page
occlusion bug above), and for that page the command has no correct target:
running it anyway returns a healthy-looking 200 about qontinui-web, which is
worse than the 404 in the table, because nothing in the response marks it as
off-target. **On the runner and on React Native there is no visibility endpoint
to reach** — record occlusion as UNVERIFIED and use `analyze`'s `occlusion`
finding, which reaches every surface that can produce a snapshot.

`verdict` is `clear`, `occlusions_found`, or `unknown_empty_registry` — the
last of which is why an empty `occlusions` list is not automatically good
news.

**A 404 is UNKNOWN, never a pass.** On React Native the route does not exist,
a web surface running an `@qontinui/ui-bridge` build older than 2026-08-27
(`4284cd29`) will 404 it, and so will a runner build older than 2026-08-30 (the
runner serves it from its own Rust twin since then). Never delete the check on a 404:
record the page's occlusion state as UNVERIFIED and fall back to `analyze`'s
`occlusion` finding (above), which reaches every surface that can produce a
snapshot.

**`includeExpected` (default `false`) drops occlusions by a TRACKED modal —
read `expectedOverlayDetection` to know whether that happened.** Filtered
entries are counted in `expectedOverlaysFiltered`; `includeExpected: true` keeps
them, marked `isExpectedOverlay: true`. The SDK has applied it since `fc6838e`
(2026-08-30, restoring the filter `ccb77d2` removed) on the handler and its
relay twin, and the runner's Rust twin does from plan
`2026-09-10-the-runners-rust-visibility-twin-diverges-from-the-sdk-it-mirrors`
onward. `"modal-stack"` means a modal detector answered and matched occluders were
filtered; a remaining `hidesText: true` entry is either a real finding or an
occluder the narrow rule could not match (a class-described backdrop, a modal
child with its own id), so check it against what is open before calling a
FAIL. `"unavailable"` — or the field ABSENT, which itself identifies a build
predating classification — means nothing was
classified: **an open modal or dropdown still reports `occlusions_found` with
`hidesText: true`**, so triage those by hand against what you know is open
rather than reading them as a regression. Matching is deliberately narrow
(registry id or DOM id equal to the modal's id), so an untracked dropdown or
popover is never filtered.

## The Assertion DSL

Each assertion is a tagged-JSON object:

| Assertion | Body shape |
|---|---|
| `no_overlap` | `{"type":"no_overlap","elements":["btn-a","btn-b"],"tolerance_px":0}` |
| `element_above` | `{"type":"element_above","elements":["dropdown","panel"],"require_overlap":true}` — asserts `elements[0]` paints **on top of** `elements[1]`. Reads the snapshot's resolved `paint_order`, **never** a raw `z-index`: a `z-50` element nested inside a `z-10` stacking context genuinely loses to a `z-20` sibling of that context, so a raw-z comparison reports the opposite of the truth. `require_overlap` defaults **true** — "which is on top" is not a question two disjoint elements answer. When the snapshot source did not resolve stacking, `paint_order` is absent and the assertion answers **"cannot answer"** rather than passing vacuously. |
| `contains_text` | `{"type":"contains_text","target":{"element":"h1"},"text":"Hello","kind":"contains"}` |
| `text_fits_container` | `{"type":"text_fits_container","element":"label-7"}` — checks **both** axes. Horizontal truncation (`scroll_width_px > bbox.w`, i.e. a `truncate` / `text-overflow: ellipsis`) is the common failure and was undetectable before; an element with no `scroll_width_px` reports the horizontal axis as UNKNOWN rather than passing silently. |
| `aligned_horizontally` | `{"type":"aligned_horizontally","elements":["a","b","c"],"axis_tolerance_px":2}` |
| `aligned_vertically` | `{"type":"aligned_vertically","elements":["nav-1","nav-2"]}` |
| `color_within` | `{"type":"color_within","element":"logo","expected":{"r":255,"g":51,"b":51},"delta_e_max":5}` |
| `typography_consistent` | `{"type":"typography_consistent","elements":["h1","h2"],"dimensions":["font_family"]}` |
| `no_layout_shift_since` | `{"type":"no_layout_shift_since","baseline":"v1.0-home","tolerance_px":2}` |
| `no_clipping` | `{"type":"no_clipping","region":{"x":0,"y":0,"w":1280,"h":720}}` |
| `animation_settled` | `{"type":"animation_settled","region":null,"settle_frames":3}` |
| `contrast_meets_wcag` | `{"type":"contrast_meets_wcag","element":"btn-save","level":"aa"}` |
| `image_palette_within` | `{"type":"image_palette_within","element":"hero","palette":[{"r":20,"g":40,"b":90}],"max_delta_e":10,"min_coverage":0.6}` — at least `min_coverage` of the image's pixels lie within `max_delta_e` (CIEDE2000) of a palette colour. Reads the frame. |
| `no_near_duplicate_images` | `{"type":"no_near_duplicate_images","max_hamming":6}` — no two images' pHashes within `max_hamming` bits. Reads the frame. |
| `no_placeholder_images` | `{"type":"no_placeholder_images"}` (or `"element":"hero"`) — no slot placeholder marker, low-entropy fill or crossed-box pattern. Reads the frame (the marker alone needs none). |
| `image_focal_visible` | `{"type":"image_focal_visible","element":"hero","focal":{"x":0.3,"y":0.2,"width":0.4,"height":0.5}}` — the focal region (natural-image fractions) survives object-fit/position. Reads media facts, not pixels. |
| `transparent_image_legible` | `{"type":"transparent_image_legible","element":"logo","min_ratio":3}` — edge and stroke pixels reach `min_ratio` WCAG contrast against the surround on this frame's theme. Reads the frame. |
| `salience_order` | `{"type":"salience_order","cta_id":"cta-signup"}` — the CTA is the most salient interactive or media element. Reads the frame. |
| `image_sharpness_at_least` | `{"type":"image_sharpness_at_least","element":"hero","min_sharpness":50}` — variance of the Laplacian at the rendered scale. Reads the frame. |
| `image_blockiness_within` | `{"type":"image_blockiness_within","element":"hero","max_blockiness":1.5}` — compression blockiness (~1.0 is clean). Reads the frame. |

Every threshold on an image assertion is the CALLER's (a slot spec or the
visual system); an omitted one answers `outcome: "unknown"`, never a pass.
`no_near_duplicate_images` and `no_placeholder_images` are page-wide and
answer `unknown` whenever an image on the page is unreadable. An image is
unreadable — never silently skipped — when, for example (the full rules are
`image::unreadable` and `image::locate` in rust-vision-core):

- the producer reports it `occluded`, `off-screen`, or not visible for a
  reason vision-core does not interpret;
- it is `visible: false` with no `visibility_reason` — which today is every
  off-screen image from a producer that does not send the reason;
- it lies wholly inside the element its `occluded_by` names;
- it is an `img`/`picture`/`video`/`canvas` with no `object_fit`, or a
  `contain` / `none` / `scale-down` fit that cannot be placed without
  `natural_px`;
- it has no bbox, or lies wholly outside the frame.

An image that paints nothing (zero-area, or reported `hidden` / `no-layout`)
is left out instead, and CSS backgrounds are left out of the page-wide checks
with a disclosed count (a slot placeholder marker on one still fails).

### Example: audit the page for the kinds of bugs Phase 6 was designed to catch

```bash
# PROJECT FIRST. `discover | jq '.data'` is not a snapshot — see the warning at
# the top of this file; it returns zero findings on a broken page.
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/control/discover \
     -H 'Content-Type: application/json' -d '{"interactive_only":false}' \
  | python3 "$ROOT/qontinui-claude-config/scripts/uibridge-to-elementsnapshot.py" \
      --stats > /tmp/snapshot.json
SNAP=$(cat /tmp/snapshot.json)

curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/assert \
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
    { "passed": false, "outcome": "failed",
      "detail": "button-terminal-1 and button-terminal-2 overlap by 1632 px²",
      "assertion": { "type": "no_overlap", "elements": ["button-terminal-1","button-terminal-2"] },
      "confidence": null },
    { "passed": true, "outcome": "passed", "assertion": { "type": "no_clipping" },
      "confidence": null },
    { "passed": true, "outcome": "passed",
      "assertion": { "type": "aligned_horizontally", "elements": [...] },
      "confidence": null }
  ],
  "allPassed": false,
  "coverage": { "elements": 214, "withGeometry": 214, "withStacking": 0,
                "withText": 118, "interactable": 63 },
  "evaluatedAt": "2026-09-07T04:31:22.418973512Z",
  "snapshotAttribution": { "state": "unattributed" },
  "frame": { "width": 2560, "height": 1440, "capturedAt": "2026-09-07T04:31:22.401884073Z",
             "scaleFactor": 1.0, "kind": "window", "captureBackend": "MonitorCrop" }
}
```

**Those `"confidence": null`s are not noise — read them as a statement.**
`confidence` carries `#[serde(default)]` and **no** `skip_serializing_if`, so it
is on the wire for every result and every finding, always. `null` means *this
verdict was DEDUCED, not estimated* — an exact geometric or textual comparison,
with nothing to be uncertain about. A NUMBER appears only where an operand was
itself estimated: `contains_text`'s OCR fallback, `color`'s pixel-sampled
contrast arm, and the image assertions that read the frame (a pixel read from
a composited frame is an estimate of the image under it). So `null` and
*absent* say different things, and only a producer predating the field omits
the key. Do not strip it and do not treat it as missing data.

**`assert` carries `coverage` too, and you must read it here for the same
reason you read it on `analyze`.** Every assertion in the DSL reads the
snapshot (the image assertions too, to find the image's box), so
`allPassed: true` over a snapshot with `withGeometry: 0` is a vacuous pass:
`no_clipping` skips every element that carries no `bbox` and then returns a
**genuine** `passed` over the emptiness it was left holding.

Each result also carries `outcome` — `"passed"`, `"failed"` or `"unknown"` —
and it is worth reading, but it does NOT catch that case. `outcome: "unknown"`
(deliberately `passed: false`) means an assertion could not be evaluated
because an INPUT was absent; the vacuous pass above is an assertion that
evaluated fine over nothing. `coverage` is what separates the two, and there is
no analyzer-level `verdict` on this route to do it for you.

Two absences here are STATEMENTS, not gaps, and `snapshotAttribution.state` is
what makes them readable:

| you see | it means |
|---|---|
| no `coverage` key, `snapshotAttribution.state: "absent"` | you sent no snapshot. Most assertions come back `outcome: "unknown"`, detail *"no ElementSnapshot supplied, so this assertion was never evaluated"* — so `allPassed` is `false`, not a vacuous `true`. Not quite all: `contains_text` against a `region` target with `ocr_blocks` supplied needs no snapshot and still returns a real pass/fail |
| `coverage` present, `snapshotAttribution.state: "unattributed"` | you sent a snapshot that carries no producer-minted id. Normal today; no producer mints one yet |
| `snapshotAttribution.state: "attributed"` | `snapshotAttribution.snapshotId` identifies the exact capture this verdict is about |

Note the presence rule differs between the two routes. On `assert`, `coverage`
is present exactly when you supplied a snapshot. On `analyze` it can be absent
even when you did, because the analyzer decides (`dynamic` never sets it).

`frame.capturedAt` dates the FRAME and only the frame. It does not date your
snapshot, so it does not tell you whether the input you posted was stale —
nothing in either response does yet. `evaluatedAt` dates the answer.

## Baselines

Register a "what good looks like" snapshot for a route:

```bash
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/baseline \
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
# PROJECT FIRST — same reason as above. Read the --stats line on stderr:
# `0 with geometry` is UNKNOWN, not a clean page.
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/control/discover \
     -H 'Content-Type: application/json' -d '{"interactive_only":false}' \
  | python3 "$ROOT/qontinui-claude-config/scripts/uibridge-to-elementsnapshot.py" \
      --stats > /tmp/snapshot.json
SNAP=$(cat /tmp/snapshot.json)

# 1. Layout analyzer — catches the overlap bugs the plan was designed for
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"layout",snapshot:$s}')" \
  | jq '.data.findings[] | select(.severity != "info")'

# 2. Elements analyzer — sub-24×24 targets + empty-snapshot smells
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"elements",snapshot:$s}')" \
  | jq '.data.findings[]'

# 3. Contrast — only meaningful when snapshot supplies fg_color + bg_color
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"color",snapshot:$s}')" \
  | jq '.data.findings[]'

# 4. A default assertion bundle for every interactive page
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/assert \
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

## The image arm — candidate assets operated in situ

Everything above audits the page as it IS. The image arm audits a page as it
WOULD BE with a candidate image in one of its slots — a generated hero, a
re-rendered logo — without rebuilding or redeploying anything. It is the arm a
candidate judge calls (plan `2026-10-13-image-generation-and-visual-judgment`,
Phase 18; judging itself arrives with that plan's Phase 20). It answers with
VALUES and frames, never a verdict on the candidate.

**It operates the page; it does not inspect a still.** A UX finding about an
image is about what a user experiences, so the arm drives the states that
experience lives in (served policy `ux-priorities`
`ux-analysis-is-performed-not-inspected` — read it via `/policy`): every
breakpoint, both colour schemes, hover, a slow network, and the
loading-to-loaded timeline. Then it **restores the page and proves it did.**

### The instrument: the SDK's media override, through `ui-bridge-inject`

`POST /control/media/override` (SDK action `mediaOverride`) swaps one slot's
image in place — an `<img>`'s `src`/`srcset`, **every** `<source srcset>` of
its `<picture>` (or per type with `sources`, the AVIF/WebP-plus-fallback
shape), or an element's CSS `background-image`. It answers only after the
browser **decoded** the candidate and `currentSrc` names it; an override it
cannot confirm is rolled back and fails (`DECODE_FAILED`, `DECODE_TIMEOUT`,
`CURRENT_SRC_MISMATCH`). A `loading="lazy"` slot is switched to eager loading
for the override's lifetime, so an offscreen slot below the fold still decodes
(and gets `lazy` back on restore). Candidate URLs must be http(s), `blob:`,
`data:image/*` or relative to the page — anything else is refused as
`UNSUPPORTED_URL_SCHEME` — and an override that a restore, the TTL or a later
override on the same slot overtook while it decoded answers
`OVERRIDE_CANCELLED`, not success. A TTL (`ttlMs`, default 120 s) restores the
page by itself if the caller dies.

`mediaOverrideRestore` puts every attribute back and then verifies what the
browser RENDERS, not what it just wrote: the slot's `currentSrc` must be back
on one of the page's original URLs (it waits for the original to load, bounded
by the override's `decodeTimeoutMs`), or, for a background, the computed
`background-image` must no longer show the candidate. **A clean restore is all
three of: `status: "restored"`, `verified: true`, and `mediaOverrideList` →
`active: []`.** `status: "restored_with_drift"` (a non-empty `drift`: the app
rewrote a value after the swap and it was left as the app set it) is never
verified, and `verified: false` with a `rendered.*` mismatch means the page may
still show the candidate — both are an **UNKNOWN restore**, never a clean
page, whatever `active` says.

**Which surfaces serve it — it is a DEV CONTROL:**

| Surface | `mediaOverride` |
|---|---|
| `ui-bridge-inject` (the injected runtime, a browser the CLI launched) | **served** — the path this arm uses |
| an embedded SDK whose app passes `features.devControls` on a dev build | served |
| any production deployment, any runner's own webview | `capability_unavailable_in_build` (typed refusal) |

A refusal is UNKNOWN for the candidate, never a pass. Target a slot by
`slotId` (its `data-ui-bridge-image-slot` value) or by a registered
`elementId`. A candidate URL must be reachable FROM THE PAGE — serve local
files over loopback, e.g. `python3 -m http.server 8090 --bind 127.0.0.1`.

Resolve the CLI the way `/verify-web` does: the monorepo build
`$ROOT/ui-bridge/packages/ui-bridge-wrapper/dist/inject-cli.cjs`, else
`node_modules/@qontinui/ui-bridge-wrapper/dist/inject-cli.cjs`, run with
`node` — never through the `.bin` symlink, through which it silently does
nothing. **Check it is new enough first:** a build without `--throttle` in its
`--help` predates this arm, and a page whose runtime predates the override
answers every `mediaOverride` with an unknown-action failure — both UNKNOWN.

```bash
INJECT="$ROOT/ui-bridge/packages/ui-bridge-wrapper/dist/inject-cli.cjs"
node "$INJECT" --help | grep -q -- '--throttle' \
  || { echo "inject-cli predates the image arm -- rebuild it"; exit 1; }
```

### 1. The capture matrix, crossed with candidates

One `--capture-matrix` run per candidate: every breakpoint × colour scheme ×
DPR (the theme toggle IS the colour-scheme axis; resizing IS the viewport
axis), each cell a fresh browser context running the same plan — swap, hover,
capture, read the slot's media facts, restore, prove it.

```bash
PAGE="$PAGE_URL"                   # the page that owns the slot
CAND=http://127.0.0.1:8090/hero-candidate-2.avif
OUT="$WORK/matrix-candidate-2"     # must be empty or absent
cat > "$WORK/plan.ndjson" <<EOF
{"action":"mediaOverride","payload":{"slotId":"hero","src":"$CAND","ttlMs":300000}}
{"action":"getControlSnapshot","payload":{}}
{"action":"find","payload":{"mediaOnly":true,"includeHidden":true}}
{"action":"executeElementAction","payload":{"id":"hero-cta","request":{"action":"hover"}}}
{"action":"getControlSnapshot","payload":{}}
{"action":"mediaOverrideRestore","payload":{}}
{"action":"mediaOverrideList","payload":{}}
EOF
node "$INJECT" --url "$PAGE" --capture-matrix "$OUT" --exec-stdin --quiet \
  --viewport 390x844,1280x800 --color-scheme light,dark --dpr 1,2 \
  < "$WORK/plan.ndjson"
```

Each cell writes `frame-<n>.png` + `snapshot-<n>.json` for every
`getControlSnapshot` (in lockstep, stamped with the DPR and colour scheme the
page itself reported), and `results.ndjson` with one line per action. Read, per
cell:

- the `mediaOverride` line: `ready.currentSrc` is the candidate and
  `ready.naturalWidth`/`naturalHeight` are its size in image PIXELS, read from
  the slot's own decoded image (`createImageBitmap`, no second request, so
  `--no-cache` does not blank it) rather than the slot's `naturalWidth`, which
  is divided by a srcset density — and Chromium keeps the slot's PREVIOUS
  density after a swap. Against the slot's rendered size × DPR, a candidate
  too small for its slot shows here before any judge looks; `null` is
  unmeasured (an engine with no `createImageBitmap`, an image with no
  intrinsic size), never small;
- the `find` line: each media row's `mediaMetadata` envelope — `objectFit`,
  `effectiveResolution` (natural ÷ rendered × DPR), `dimsReserved`,
  `isLcpCandidate`, `clsSources`, and `encodedBodySize` keyed on `currentSrc`;
  a dimension it could not measure sits in `coverage.unmeasured` with its code,
  never as `0`. The rows are the PAGE's registrations, so a page whose own SDK
  predates that envelope answers the old shape (an `oversizeRatio`, no
  `coverage` — what qontinui.io served on 2026-10-19): there every one of those
  facts is UNKNOWN, never "absent, so fine";
- the last two lines: every `restored[]` entry `status: "restored"` with
  `verified: true`, and `mediaOverrideList` → `active: []`. **A cell whose list
  is not empty did not end restored — report it, do not average it away.** An
  entry with a non-empty `drift` (`status: "restored_with_drift"`) or
  `verified: false` is an UNKNOWN restore for that cell even when `active` is
  empty: `drift` names a value the app rewrote after the swap (left as the app
  set it, so the slot shows the app's choice, possibly still the candidate),
  and a `rendered.currentSrc` / `rendered.background-image` mismatch says the
  browser still renders something other than the original.

Hover targets are page-specific — take the id from the cell's own snapshot
(the slot's link or card ancestor), never from this example.

### 2. A slow network, and the loading-to-loaded timeline

`--throttle` (`slow-3g`, `fast-3g`, or `latency=<ms>,down=<kbps>,up=<kbps>`)
applies Chromium network emulation once the page is ready; `--no-cache` stops a
warm cache from painting a "slow" load instantly. What Chromium accepted is
stamped as `network` on every timeline result and matrix cell.

`captureTimeline` records frames on a timer. Two shapes answer two questions:

```bash
# (a) The candidate ARRIVING in its slot over a slow network: the override runs
#     concurrently with the frames, and still confirms decode before it answers.
#     slow-3g is 2 s latency + 400 kbit/s (50 KB/s), so a 300 KB candidate takes
#     ~8 s: decodeTimeoutMs well above that (the default 10 s is not), and
#     frames x intervalMs (here 40 x 500 = 20 s) long enough to see it land.
node "$INJECT" --url "$PAGE" --throttle slow-3g --no-cache --quiet \
  --exec "captureTimeline {\"dir\":\"$WORK/arrival\",\"frames\":40,\"intervalMs\":500,\"during\":{\"action\":\"mediaOverride\",\"payload\":{\"slotId\":\"hero\",\"src\":\"$CAND\",\"decodeTimeoutMs\":45000}}}" \
  --exec 'mediaOverrideRestore {}' --exec 'mediaOverrideList {}'

# (b) The slot's own loading-to-loaded timeline: reload under the throttle,
#     frame 1 at the navigation commit, then read LCP/CLS from the media facts.
node "$INJECT" --url "$PAGE" --throttle slow-3g --no-cache --quiet \
  --exec "captureTimeline {\"dir\":\"$WORK/load\",\"frames\":40,\"intervalMs\":500,\"reload\":true}" \
  --exec 'find {"mediaOnly":true,"includeHidden":true}'
```

Size the timeline from the candidate's bytes: at 50 KB/s plus 2 s latency, the
last frame must come after the arrival, or the frames record only the empty box
and the "arrival" is unobserved. (a)'s `during.answeredAtMs` against the last
frame's `offsetMs` says whether it did. The restore after a `--no-cache` run
re-downloads the original under the same throttle; its wait is bounded by that
`decodeTimeoutMs` too, so a short one there reads as `verified: false` — an
UNKNOWN restore while the original is still in flight, not proof either way.

(a)'s `during.answeredAtMs` is when the candidate was confirmed on screen and
its frames show what stood in the slot until then (an empty box, the old image,
a layout jump). (b) needs no override — a reload replaces the document, which
drops any override — and answers whether the slot reserves its box (`clsSources`
naming the slot is a measured layout shift) and whether it is the LCP element.
Frames are raw pixels: `data-bridge-redact` does not mask a screenshot.

### 3. What the arm hands on

Per candidate: the matrix manifest (`$OUT/matrix.json`), the arrival and load
timelines, and the restore proof. Phase 20's judges compare candidates from
these artifacts; nothing in this arm ranks them. When any precondition fails —
the CLI too old, the page's runtime refusing the override, a cell not
`captured`, a list not empty — the candidate's in-situ state is **UNKNOWN**,
and the report says which precondition failed.

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
