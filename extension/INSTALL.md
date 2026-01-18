# Qontinui DOM Capture Extension

A browser extension for capturing page DOM and sending it to qontinui-runner for AI debugging.

## Download

**[Download Extension (ZIP)](https://qontinui.io/downloads/qontinui-dom-capture-extension.zip)**

## Supported Browsers

- Google Chrome
- Microsoft Edge
- Brave
- Other Chromium-based browsers

## Installation

### Step 1: Download and Extract

1. Download the extension ZIP file from the link above
2. Extract the ZIP to a folder on your computer (e.g., `C:\qontinui-extension` or `~/qontinui-extension`)
3. Remember this location - you'll need it in Step 3

### Step 2: Open Extensions Page

**Chrome:**
- Navigate to `chrome://extensions/` in your address bar
- Or: Menu (⋮) → Extensions → Manage Extensions

**Edge:**
- Navigate to `edge://extensions/` in your address bar
- Or: Menu (⋯) → Extensions → Manage Extensions

**Brave:**
- Navigate to `brave://extensions/` in your address bar

### Step 3: Enable Developer Mode

Toggle **"Developer mode"** in the top-right corner of the extensions page.

### Step 4: Load the Extension

1. Click **"Load unpacked"** button (appears after enabling Developer mode)
2. Navigate to the folder where you extracted the extension
3. Select the folder and click "Select Folder"

### Step 5: Verify Installation

You should see the Qontinui DOM Capture extension in your extensions list with a blue icon. You can pin it to your toolbar for easy access.

## Usage

### Prerequisites

- **qontinui-runner** must be running on your computer (port 9876)
- The runner's MCP API server should be active

### Capturing DOM

1. Navigate to the web page you want to capture
2. Click the Qontinui extension icon in your toolbar
3. The popup will show connection status:
   - **Green**: Runner connected - ready to capture
   - **Red**: Runner not available - start qontinui-runner first

4. Choose capture type:
   - **Capture Full Page**: Captures the entire page HTML
   - **Capture Selector**: Enter a CSS selector (e.g., `#main`, `.content`) to capture specific elements

5. After capture:
   - Success message shows the captured HTML size
   - View captures in qontinui-runner → AI Workflows → Monitor → DOM Snapshots tab

## Troubleshooting

### "Runner not available"

- Ensure qontinui-runner is running
- Check that the MCP API server is active on port 9876
- Try restarting qontinui-runner

### "Element not found"

- The CSS selector didn't match any elements on the page
- Check your selector syntax
- Try a simpler selector like `body` or `#app`

### Extension not appearing

- Make sure Developer mode is enabled
- Try clicking "Load unpacked" again
- Check that you selected the correct folder (the one containing `manifest.json`)

### Permission errors

- Some pages (like `chrome://` URLs) cannot be captured
- Try capturing on a regular website

## Updating the Extension

1. Download the new version ZIP
2. Extract to the same folder (overwriting existing files)
3. Go to the extensions page
4. Click the refresh icon (↻) on the Qontinui extension card

## Uninstalling

1. Go to the extensions page (`chrome://extensions/` or `edge://extensions/`)
2. Find Qontinui DOM Capture
3. Click "Remove"
4. Optionally, delete the extracted folder from your computer

## Privacy

This extension:
- Only activates when you click the extension icon
- Only sends data to `localhost:9876` (your local qontinui-runner)
- Does not collect or transmit any data to external servers
- Does not track your browsing activity

## Support

For issues or feature requests, visit:
https://github.com/qontinui/qontinui-runner/issues
