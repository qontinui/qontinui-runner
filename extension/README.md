# Qontinui DOM Capture Extension

A Chrome/Edge extension for capturing page DOM and sending it to qontinui-runner for AI debugging.

> **End Users:** See [INSTALL.md](./INSTALL.md) for installation instructions.

## Development

### Loading for Development

1. Open Chrome/Edge and navigate to `chrome://extensions/` or `edge://extensions/`
2. Enable "Developer mode" (toggle in top right)
3. Click "Load unpacked"
4. Select this `extension` folder

### Icons

The extension requires PNG icons at these sizes:
- `icons/icon16.png` (16x16) - Toolbar
- `icons/icon48.png` (48x48) - Extensions page
- `icons/icon128.png` (128x128) - Web Store

**Note:** Create branded PNG icons for production. Placeholder icons are included for development.

## Usage

1. Ensure qontinui-runner is running (port 9876)
2. Click the extension icon in your browser toolbar
3. If connected, click "Capture Full Page" or enter a CSS selector for targeted capture
4. View captured DOM in the runner's Monitor > DOM Snapshots tab

## Features

- **Full Page Capture**: Captures the entire `document.documentElement.outerHTML`
- **Selector Capture**: Captures only elements matching a CSS selector
- **Connection Status**: Shows whether the runner is available
- **Size Display**: Shows the size of captured HTML

## API

The extension sends POST requests to `http://localhost:9876/dom/receive` with:

```json
{
  "url": "https://example.com/page",
  "pageTitle": "Page Title",
  "html": "<html>...</html>",
  "selector": null,
  "taskRunId": null
}
```

## Troubleshooting

- **"Runner not available"**: Ensure qontinui-runner is running and the MCP API server is active on port 9876
- **"Element not found"**: The CSS selector didn't match any elements on the page
- **Permission errors**: Make sure the extension has permission to access the current tab
