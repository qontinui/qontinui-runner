# ui_bridge_visual_assertion

Visual assertion step type for the UI Bridge automation module. Delegates to the `@qontinui/ui-bridge-auto` visual endpoints for text assertions (DOM/OCR), screenshot comparison, and element highlighting.

## Assertion Types

### Text Assertion

Asserts that an element contains expected text. Uses DOM `textContent` for standard elements and OCR for media elements (canvas, img, video, svg).

```yaml
- type: ui_bridge_visual_assertion
  name: Verify login button text
  config:
    visualAssertionType: text
    visualAssertionQuery:
      id: "login-btn"
    visualAssertionExpected: "Log In"
    visualAssertionOptions:
      caseSensitive: false
      fuzzyThreshold: 0.9
```

### Screenshot Assertion

Compares an element's current appearance against a stored baseline screenshot. Uses pixel-level comparison with configurable thresholds.

```yaml
- type: ui_bridge_visual_assertion
  name: Check header visual regression
  config:
    visualAssertionType: screenshot
    visualAssertionExpected: "header-element-id"
    visualAssertionOptions:
      pixelThreshold: 15
      failureThreshold: 0.5
      failureThresholdType: percent
      updateBaseline: true
```

To capture a baseline first (no comparison):

```yaml
- type: ui_bridge_visual_assertion
  name: Capture header baseline
  config:
    visualAssertionType: screenshot
    visualAssertionExpected: "header-element-id"
    visualAssertionOptions:
      updateBaseline: true
```

### Element Highlight

Highlights an element with a visual overlay for debugging. Does not assert — always succeeds if the element exists.

```yaml
- type: ui_bridge_visual_assertion
  name: Flash the submit button
  config:
    visualAssertionType: highlight
    visualAssertionExpected: "submit-btn"
    visualAssertionOptions:
      color: "#ff0000"
      duration: 2000
      flash: true
      label: "Submit button"
```

## Configuration Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `visualAssertionType` | `"text" \| "screenshot" \| "highlight"` | Yes | Which assertion to run |
| `visualAssertionQuery` | `object` | For text | Element query (id, text, role, etc.) |
| `visualAssertionExpected` | `string` | Yes | Expected text (text) or element ID (screenshot/highlight) |
| `visualAssertionOptions` | `object` | No | Type-specific options (see below) |

### Text Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `caseSensitive` | `boolean` | `false` | Case-sensitive comparison |
| `fuzzyThreshold` | `number` | `0.8` | Minimum similarity (0-1) for fuzzy match |
| `timeout` | `number` | `0` | Retry timeout in ms (0 = no retry) |

### Screenshot Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `pixelThreshold` | `number` | `10` | Per-pixel color tolerance (0-255) |
| `failureThreshold` | `number` | `0.1` | Maximum allowed diff percentage |
| `failureThresholdType` | `"percent" \| "pixel"` | `"percent"` | How to interpret failureThreshold |
| `blur` | `number` | `0` | Anti-aliasing blur radius |
| `updateBaseline` | `boolean` | `false` | Save current as new baseline |
| `baselineKey` | `string` | auto | Custom baseline storage key |
| `maskRegions` | `array` | none | Regions to exclude from comparison |

### Highlight Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `color` | `string` | `"#00c800"` | CSS color for the highlight border |
| `duration` | `number` | `800` | Duration in ms before auto-dismiss |
| `thickness` | `number` | `3` | Border thickness in px |
| `flash` | `boolean` | `false` | Enable blink animation |
| `label` | `string` | none | Text label above the highlight |

## Requirements

- SDK app must be connected (UI Bridge SDK running)
- For OCR text assertions: `tesseract.js` must be installed in the SDK app
- For screenshot assertions: baselines are stored in memory by default (use IndexedDB store for persistence)

## HTTP Endpoints

The step handler calls these runner relay endpoints, which proxy to the SDK app:

- `POST /ui-bridge/sdk/auto/assertText`
- `POST /ui-bridge/sdk/auto/assertScreenshot`
- `POST /ui-bridge/sdk/auto/highlightElement`
