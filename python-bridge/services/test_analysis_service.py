"""
Test Analysis Service for AI-powered test generation.

Provides page analysis capabilities for the Test Builder:
- Playwright CDP analysis: Connects to existing browser via CDP, extracts DOM elements
- Vision analysis: Uses OCR, edge detection, and segmentation to detect UI elements
- Annotated screenshot generation: Creates numbered overlays for element reference

The analysis data is used by AI to generate test code from natural language descriptions.
"""

import base64
import io
import logging
import uuid
from datetime import datetime
from typing import Any, Literal

import cv2
import numpy as np
from PIL import Image, ImageDraw, ImageFont

logger = logging.getLogger(__name__)


class TestAnalysisService:
    """
    Service for analyzing pages to support AI test generation.

    Supports two analysis modes:
    - Playwright: Connects to browser via CDP, extracts DOM elements with selectors
    - Vision: Uses computer vision (OCR, edge, SAM3) for visual element detection
    """

    def __init__(self, event_manager=None, vision_service=None):
        """
        Initialize the test analysis service.

        Args:
            event_manager: EventManager for emitting events to Rust bridge.
            vision_service: VisionExtractionService instance for vision analysis.
        """
        self.event_manager = event_manager
        self.vision_service = vision_service
        self._is_running = False
        self._current_analysis_id: str | None = None

    async def analyze_via_playwright(self, cdp_port: int = 9222) -> dict[str, Any]:
        """
        Analyze page by connecting to existing browser via CDP.

        Args:
            cdp_port: Chrome DevTools Protocol port (default 9222).

        Returns:
            PageAnalysis dict with screenshot, annotated screenshot, and elements.
        """
        import time

        start_time = time.time()
        analysis_id = str(uuid.uuid4())
        self._current_analysis_id = analysis_id
        self._is_running = True

        try:
            # Try to import playwright
            try:
                from playwright.async_api import async_playwright
            except ImportError:
                return {
                    "success": False,
                    "error": "Playwright not installed. Run: pip install playwright && playwright install chromium",
                }

            async with async_playwright() as p:
                # Connect to existing browser via CDP
                try:
                    browser = await p.chromium.connect_over_cdp(
                        f"http://127.0.0.1:{cdp_port}"
                    )
                except Exception as e:
                    return {
                        "success": False,
                        "error": f"Failed to connect to browser via CDP on port {cdp_port}. "
                        f"Make sure Chrome is running with --remote-debugging-port={cdp_port}. "
                        f"Error: {str(e)}",
                    }

                # Get the first context and page
                contexts = browser.contexts
                if not contexts:
                    return {
                        "success": False,
                        "error": "No browser contexts found. Make sure Chrome has at least one window open.",
                    }

                context = contexts[0]
                pages = context.pages
                if not pages:
                    return {
                        "success": False,
                        "error": "No pages found in browser. Make sure at least one tab is open.",
                    }

                page = pages[0]
                url = page.url

                # Capture screenshot
                screenshot_bytes = await page.screenshot(type="png")
                screenshot_base64 = base64.b64encode(screenshot_bytes).decode()

                # Extract DOM elements
                elements = await self._extract_dom_elements(page)

                # Generate annotated screenshot
                annotated_screenshot_base64 = self._generate_annotated_screenshot(
                    screenshot_bytes, elements
                )

                processing_time_ms = (time.time() - start_time) * 1000

                return {
                    "success": True,
                    "data": {
                        "analysis_id": analysis_id,
                        "screenshot_base64": screenshot_base64,
                        "annotated_screenshot_base64": annotated_screenshot_base64,
                        "elements": elements,
                        "source": "playwright",
                        "captured_at": datetime.utcnow().isoformat(),
                        "url": url,
                        "processing_time_ms": processing_time_ms,
                    },
                }

        except Exception as e:
            logger.error(f"Playwright analysis failed: {e}", exc_info=True)
            return {"success": False, "error": str(e)}
        finally:
            self._is_running = False
            self._current_analysis_id = None

    async def analyze_via_playwright_script(self, script: str) -> dict[str, Any]:
        """
        Analyze page by running a Playwright script.

        The script should navigate to the target page. After the script completes,
        DOM elements are extracted from the resulting page state.

        Args:
            script: Playwright TypeScript/JavaScript code that navigates to the page.

        Returns:
            PageAnalysis dict with screenshot, annotated screenshot, and elements.
        """
        import tempfile
        import time
        import subprocess
        import os
        import json

        start_time = time.time()
        analysis_id = str(uuid.uuid4())
        self._current_analysis_id = analysis_id
        self._is_running = True

        try:
            # Try to import playwright
            try:
                from playwright.async_api import async_playwright
            except ImportError:
                return {
                    "success": False,
                    "error": "Playwright not installed. Run: pip install playwright && playwright install chromium",
                }

            # Create a wrapper script that runs the user's script and then captures elements
            wrapper_script = f'''
const {{ chromium }} = require('playwright');

(async () => {{
    let browser;
    let page;

    try {{
        // User's script logic - extract URL if present
        const userScript = `{script.replace('`', '\\`').replace('$', '\\$')}`;

        // Parse user script to find goto URL
        const gotoMatch = userScript.match(/goto\\s*\\(\\s*['"]([^'"]+)['"]/);
        const targetUrl = gotoMatch ? gotoMatch[1] : null;

        if (!targetUrl) {{
            console.error(JSON.stringify({{ success: false, error: "Script must contain page.goto('url')" }}));
            process.exit(1);
        }}

        // Launch browser
        browser = await chromium.launch({{ headless: false }});
        const context = await browser.newContext();
        page = await context.newPage();

        // Navigate to the URL
        await page.goto(targetUrl);
        await page.waitForLoadState('networkidle');

        // Capture screenshot
        const screenshot = await page.screenshot({{ type: 'png' }});
        const screenshotBase64 = screenshot.toString('base64');

        // Extract elements using the same JS as CDP method
        const elements = await page.evaluate(() => {{
            const interactiveSelectors = [
                'button', 'a', 'input', 'select', 'textarea',
                '[role="button"]', '[role="link"]', '[role="textbox"]',
                '[role="checkbox"]', '[role="radio"]', '[role="combobox"]',
                '[role="menuitem"]', '[role="tab"]', '[role="slider"]',
                '[onclick]', '[data-testid]', '[data-cy]', '[data-test]'
            ];

            const results = [];
            const seen = new Set();

            for (const selector of interactiveSelectors) {{
                const els = document.querySelectorAll(selector);
                for (const el of els) {{
                    const rect = el.getBoundingClientRect();
                    if (rect.width === 0 || rect.height === 0) continue;

                    const style = window.getComputedStyle(el);
                    if (style.display === 'none' || style.visibility === 'hidden') continue;
                    if (parseFloat(style.opacity) < 0.1) continue;

                    const key = `${{el.tagName}}-${{rect.x}}-${{rect.y}}-${{rect.width}}-${{rect.height}}`;
                    if (seen.has(key)) continue;
                    seen.add(key);

                    let bestSelector = '';
                    if (el.id) bestSelector = `#${{el.id}}`;
                    else if (el.dataset.testid) bestSelector = `[data-testid="${{el.dataset.testid}}"]`;
                    else if (el.dataset.cy) bestSelector = `[data-cy="${{el.dataset.cy}}"]`;
                    else if (el.className && typeof el.className === 'string') {{
                        const classes = el.className.trim().split(/\\s+/).slice(0, 2).join('.');
                        if (classes) bestSelector = `${{el.tagName.toLowerCase()}}.${{classes}}`;
                    }}
                    if (!bestSelector) {{
                        bestSelector = el.tagName.toLowerCase();
                        if (el.type) bestSelector += `[type="${{el.type}}"]`;
                    }}

                    let text = '';
                    if (el.value) text = el.value;
                    else if (el.textContent) text = el.textContent.trim().substring(0, 100);
                    else if (el.getAttribute('aria-label')) text = el.getAttribute('aria-label');

                    let elType = el.tagName.toLowerCase();
                    if (el.type) elType = el.type;
                    if (el.getAttribute('role')) elType = el.getAttribute('role');

                    results.push({{
                        tagName: el.tagName.toLowerCase(),
                        type: elType,
                        selector: bestSelector,
                        text: text,
                        bbox: {{
                            x: Math.round(rect.x),
                            y: Math.round(rect.y),
                            width: Math.round(rect.width),
                            height: Math.round(rect.height)
                        }}
                    }});
                }}
            }}
            return results;
        }});

        const url = page.url();

        console.log(JSON.stringify({{
            success: true,
            screenshotBase64,
            elements,
            url
        }}));

    }} catch (error) {{
        console.error(JSON.stringify({{ success: false, error: error.message }}));
        process.exit(1);
    }} finally {{
        if (browser) await browser.close();
    }}
}})();
'''

            # Write wrapper script to temp file
            with tempfile.NamedTemporaryFile(mode='w', suffix='.js', delete=False) as f:
                f.write(wrapper_script)
                script_path = f.name

            try:
                # Run the script with Node.js
                result = subprocess.run(
                    ['node', script_path],
                    capture_output=True,
                    text=True,
                    timeout=90,
                    cwd=os.path.dirname(script_path)
                )

                if result.returncode != 0:
                    error_output = result.stderr or result.stdout
                    # Try to parse JSON error
                    try:
                        error_data = json.loads(error_output.strip().split('\n')[-1])
                        return {"success": False, "error": error_data.get("error", error_output)}
                    except:
                        return {"success": False, "error": f"Script failed: {error_output}"}

                # Parse output
                output_lines = result.stdout.strip().split('\n')
                for line in reversed(output_lines):
                    try:
                        data = json.loads(line)
                        if data.get("success"):
                            # Convert elements to our format
                            elements = []
                            for i, raw in enumerate(data.get("elements", [])):
                                element_id = f"element_{i + 1}"
                                label = raw.get("text", "")[:30] or raw.get("type", "element")
                                elements.append({
                                    "id": element_id,
                                    "label": label,
                                    "element_type": raw.get("type", "element"),
                                    "text_content": raw.get("text"),
                                    "bounding_box": raw.get("bbox"),
                                    "confidence": 1.0,
                                    "selector": raw.get("selector"),
                                    "attributes": {},
                                })

                            # Generate annotated screenshot
                            screenshot_bytes = base64.b64decode(data.get("screenshotBase64", ""))
                            annotated_screenshot_base64 = self._generate_annotated_screenshot(
                                screenshot_bytes, elements
                            )

                            processing_time_ms = (time.time() - start_time) * 1000

                            return {
                                "success": True,
                                "data": {
                                    "analysis_id": analysis_id,
                                    "screenshot_base64": data.get("screenshotBase64"),
                                    "annotated_screenshot_base64": annotated_screenshot_base64,
                                    "elements": elements,
                                    "source": "playwright_script",
                                    "captured_at": datetime.utcnow().isoformat(),
                                    "url": data.get("url"),
                                    "processing_time_ms": processing_time_ms,
                                },
                            }
                        else:
                            return {"success": False, "error": data.get("error", "Unknown error")}
                    except json.JSONDecodeError:
                        continue

                return {"success": False, "error": "No valid output from script"}

            finally:
                # Clean up temp file
                try:
                    os.unlink(script_path)
                except:
                    pass

        except subprocess.TimeoutExpired:
            return {
                "success": False,
                "error": "Script execution timed out after 90 seconds",
            }
        except Exception as e:
            logger.error(f"Playwright script analysis failed: {e}", exc_info=True)
            return {"success": False, "error": str(e)}
        finally:
            self._is_running = False
            self._current_analysis_id = None

    async def _extract_dom_elements(self, page) -> list[dict[str, Any]]:
        """
        Extract interactive DOM elements from the page.

        Returns list of DetectedElement dicts with:
        - id, label, element_type, text_content, bounding_box, confidence, selector, attributes
        """
        elements = []

        # JavaScript to extract interactive elements
        js_code = """
        () => {
            const interactiveSelectors = [
                'button', 'a', 'input', 'select', 'textarea',
                '[role="button"]', '[role="link"]', '[role="textbox"]',
                '[role="checkbox"]', '[role="radio"]', '[role="combobox"]',
                '[role="menuitem"]', '[role="tab"]', '[role="slider"]',
                '[onclick]', '[data-testid]', '[data-cy]', '[data-test]'
            ];

            const elements = [];
            const seen = new Set();

            for (const selector of interactiveSelectors) {
                const els = document.querySelectorAll(selector);
                for (const el of els) {
                    // Skip hidden elements
                    const rect = el.getBoundingClientRect();
                    if (rect.width === 0 || rect.height === 0) continue;

                    const style = window.getComputedStyle(el);
                    if (style.display === 'none' || style.visibility === 'hidden') continue;
                    if (parseFloat(style.opacity) < 0.1) continue;

                    // Generate unique key
                    const key = `${el.tagName}-${rect.x}-${rect.y}-${rect.width}-${rect.height}`;
                    if (seen.has(key)) continue;
                    seen.add(key);

                    // Get selector
                    let bestSelector = '';
                    if (el.id) {
                        bestSelector = `#${el.id}`;
                    } else if (el.dataset.testid) {
                        bestSelector = `[data-testid="${el.dataset.testid}"]`;
                    } else if (el.dataset.cy) {
                        bestSelector = `[data-cy="${el.dataset.cy}"]`;
                    } else if (el.className && typeof el.className === 'string') {
                        const classes = el.className.trim().split(/\\s+/).slice(0, 2).join('.');
                        if (classes) bestSelector = `${el.tagName.toLowerCase()}.${classes}`;
                    }
                    if (!bestSelector) {
                        bestSelector = el.tagName.toLowerCase();
                        if (el.type) bestSelector += `[type="${el.type}"]`;
                    }

                    // Get text content
                    let text = '';
                    if (el.value) text = el.value;
                    else if (el.textContent) text = el.textContent.trim().substring(0, 100);
                    else if (el.getAttribute('aria-label')) text = el.getAttribute('aria-label');
                    else if (el.getAttribute('title')) text = el.getAttribute('title');
                    else if (el.getAttribute('placeholder')) text = el.getAttribute('placeholder');

                    // Get element type
                    let elType = el.tagName.toLowerCase();
                    if (el.type) elType = el.type;
                    if (el.getAttribute('role')) elType = el.getAttribute('role');

                    elements.push({
                        tagName: el.tagName.toLowerCase(),
                        type: elType,
                        selector: bestSelector,
                        text: text,
                        bbox: {
                            x: Math.round(rect.x),
                            y: Math.round(rect.y),
                            width: Math.round(rect.width),
                            height: Math.round(rect.height)
                        },
                        attributes: {
                            id: el.id || null,
                            class: el.className || null,
                            name: el.name || null,
                            type: el.type || null,
                            placeholder: el.getAttribute('placeholder') || null,
                            ariaLabel: el.getAttribute('aria-label') || null,
                            role: el.getAttribute('role') || null,
                            testId: el.dataset.testid || el.dataset.cy || el.dataset.test || null
                        }
                    });
                }
            }

            return elements;
        }
        """

        raw_elements = await page.evaluate(js_code)

        # Convert to our format
        for i, raw in enumerate(raw_elements):
            element_id = f"element_{i + 1}"

            # Generate human-readable label
            label = raw.get("text", "")[:30] or raw.get("type", "element")
            if raw.get("attributes", {}).get("testId"):
                label = raw["attributes"]["testId"]
            elif raw.get("attributes", {}).get("id"):
                label = raw["attributes"]["id"]

            elements.append(
                {
                    "id": element_id,
                    "label": label,
                    "element_type": raw.get("type", "element"),
                    "text_content": raw.get("text"),
                    "bounding_box": raw.get("bbox"),
                    "confidence": 1.0,  # DOM elements have perfect confidence
                    "selector": raw.get("selector"),
                    "attributes": raw.get("attributes", {}),
                }
            )

        logger.info(f"Extracted {len(elements)} interactive elements from DOM")
        return elements

    def analyze_via_vision(
        self, screenshot_base64: str = None, monitor_index: int = 0
    ) -> dict[str, Any]:
        """
        Analyze page using computer vision (OCR, edge detection, segmentation).

        Args:
            screenshot_base64: Pre-captured screenshot, or None to capture from monitor.
            monitor_index: Monitor index to capture if screenshot not provided.

        Returns:
            PageAnalysis dict with screenshot, annotated screenshot, and elements.
        """
        import time

        start_time = time.time()
        analysis_id = str(uuid.uuid4())
        self._current_analysis_id = analysis_id
        self._is_running = True

        try:
            # Get screenshot
            if screenshot_base64:
                screenshot_bytes = base64.b64decode(
                    screenshot_base64.split(",")[-1]
                    if "," in screenshot_base64
                    else screenshot_base64
                )
            else:
                # Capture from monitor
                screenshot_bytes = self._capture_monitor(monitor_index)
                if screenshot_bytes is None:
                    return {
                        "success": False,
                        "error": f"Failed to capture screenshot from monitor {monitor_index}",
                    }
                screenshot_base64 = base64.b64encode(screenshot_bytes).decode()

            # Use vision extraction service if available
            if self.vision_service:
                vision_result = self.vision_service.extract(
                    {
                        "screenshot": screenshot_base64,
                        "techniques": ["edge", "ocr"],  # Skip SAM3 for speed
                        "min_contour_area": 500,
                        "ocr_confidence_threshold": 0.3,
                    }
                )

                if not vision_result.get("success"):
                    return vision_result

                vision_data = vision_result.get("data", {})

                # Convert vision results to elements
                elements = self._convert_vision_to_elements(vision_data)
            else:
                # Fallback: just use OCR
                elements = self._simple_ocr_analysis(screenshot_bytes)

            # Generate annotated screenshot
            annotated_screenshot_base64 = self._generate_annotated_screenshot(
                screenshot_bytes, elements
            )

            processing_time_ms = (time.time() - start_time) * 1000

            return {
                "success": True,
                "data": {
                    "analysis_id": analysis_id,
                    "screenshot_base64": screenshot_base64,
                    "annotated_screenshot_base64": annotated_screenshot_base64,
                    "elements": elements,
                    "source": "vision",
                    "captured_at": datetime.utcnow().isoformat(),
                    "monitor_index": monitor_index,
                    "processing_time_ms": processing_time_ms,
                },
            }

        except Exception as e:
            logger.error(f"Vision analysis failed: {e}", exc_info=True)
            return {"success": False, "error": str(e)}
        finally:
            self._is_running = False
            self._current_analysis_id = None

    def _capture_monitor(self, monitor_index: int) -> bytes | None:
        """Capture screenshot from specified monitor."""
        try:
            import mss

            with mss.mss() as sct:
                monitors = sct.monitors
                if monitor_index < 0 or monitor_index >= len(monitors):
                    monitor_index = 0

                # monitors[0] is all monitors combined, [1] is first, etc.
                monitor = monitors[monitor_index + 1] if monitor_index + 1 < len(monitors) else monitors[1]

                screenshot = sct.grab(monitor)
                img = Image.frombytes("RGB", screenshot.size, screenshot.bgra, "raw", "BGRX")

                buffer = io.BytesIO()
                img.save(buffer, format="PNG")
                return buffer.getvalue()

        except ImportError:
            logger.error("mss not installed. Run: pip install mss")
            return None
        except Exception as e:
            logger.error(f"Failed to capture monitor: {e}")
            return None

    def _convert_vision_to_elements(self, vision_data: dict) -> list[dict[str, Any]]:
        """Convert vision extraction results to element format."""
        elements = []
        element_counter = 1

        # Process merged candidates
        for candidate in vision_data.get("merged_candidates", []):
            bbox = candidate.get("bbox", {})

            # Generate label from text or category
            label = candidate.get("text") or candidate.get("category", "element")
            if isinstance(label, str):
                label = label[:30]

            elements.append(
                {
                    "id": f"element_{element_counter}",
                    "label": label,
                    "element_type": candidate.get("category", "element"),
                    "text_content": candidate.get("text"),
                    "bounding_box": {
                        "x": bbox.get("x", 0),
                        "y": bbox.get("y", 0),
                        "width": bbox.get("width", 0),
                        "height": bbox.get("height", 0),
                    },
                    "confidence": candidate.get("confidence", 0.5),
                    "selector": None,  # Vision analysis doesn't provide selectors
                    "attributes": {
                        "detection_technique": candidate.get("detection_technique"),
                        "is_clickable": candidate.get("is_clickable", False),
                    },
                }
            )
            element_counter += 1

        logger.info(f"Converted {len(elements)} vision candidates to elements")
        return elements

    def _simple_ocr_analysis(self, screenshot_bytes: bytes) -> list[dict[str, Any]]:
        """Simple fallback OCR analysis when vision service is not available."""
        elements = []

        try:
            import easyocr

            # Decode image
            nparr = np.frombuffer(screenshot_bytes, np.uint8)
            img = cv2.imdecode(nparr, cv2.IMREAD_COLOR)
            img_rgb = cv2.cvtColor(img, cv2.COLOR_BGR2RGB)

            # Run OCR
            reader = easyocr.Reader(["en"], gpu=False)
            results = reader.readtext(img_rgb)

            for i, (bbox_pts, text, confidence) in enumerate(results):
                if confidence < 0.3:
                    continue

                pts = np.array(bbox_pts)
                x = int(min(pts[:, 0]))
                y = int(min(pts[:, 1]))
                w = int(max(pts[:, 0]) - x)
                h = int(max(pts[:, 1]) - y)

                elements.append(
                    {
                        "id": f"element_{i + 1}",
                        "label": text[:30],
                        "element_type": "text",
                        "text_content": text,
                        "bounding_box": {"x": x, "y": y, "width": w, "height": h},
                        "confidence": confidence,
                        "selector": None,
                        "attributes": {"detection_technique": "ocr"},
                    }
                )

        except Exception as e:
            logger.error(f"Simple OCR analysis failed: {e}")

        return elements

    def _generate_annotated_screenshot(
        self, screenshot_bytes: bytes, elements: list[dict[str, Any]]
    ) -> str:
        """
        Generate annotated screenshot with numbered element overlays.

        Args:
            screenshot_bytes: Original screenshot as PNG bytes.
            elements: List of detected elements with bounding boxes.

        Returns:
            Base64-encoded annotated PNG image.
        """
        try:
            # Load image
            img = Image.open(io.BytesIO(screenshot_bytes)).convert("RGBA")
            draw = ImageDraw.Draw(img)

            # Try to load a font, fall back to default
            try:
                font = ImageFont.truetype("arial.ttf", 14)
                small_font = ImageFont.truetype("arial.ttf", 10)
            except OSError:
                font = ImageFont.load_default()
                small_font = font

            # Colors for different element types
            colors = {
                "button": (0, 200, 83, 200),  # Green
                "input": (33, 150, 243, 200),  # Blue
                "link": (156, 39, 176, 200),  # Purple
                "text": (255, 152, 0, 200),  # Orange
                "container": (158, 158, 158, 200),  # Gray
                "element": (255, 87, 34, 200),  # Deep orange
            }

            for i, element in enumerate(elements):
                bbox = element.get("bounding_box", {})
                x = bbox.get("x", 0)
                y = bbox.get("y", 0)
                w = bbox.get("width", 0)
                h = bbox.get("height", 0)

                # Get color based on element type
                el_type = element.get("element_type", "element").lower()
                color = colors.get(el_type, colors["element"])

                # Draw bounding box
                draw.rectangle([x, y, x + w, y + h], outline=color[:3], width=2)

                # Draw element number badge
                badge_text = str(i + 1)
                badge_bbox = draw.textbbox((0, 0), badge_text, font=font)
                badge_w = badge_bbox[2] - badge_bbox[0] + 8
                badge_h = badge_bbox[3] - badge_bbox[1] + 4

                badge_x = max(0, x - 2)
                badge_y = max(0, y - badge_h - 2)

                # Draw badge background
                draw.rectangle(
                    [badge_x, badge_y, badge_x + badge_w, badge_y + badge_h],
                    fill=color,
                )
                # Draw badge text
                draw.text(
                    (badge_x + 4, badge_y + 2),
                    badge_text,
                    fill=(255, 255, 255),
                    font=font,
                )

            # Convert to base64
            buffer = io.BytesIO()
            img.save(buffer, format="PNG")
            return base64.b64encode(buffer.getvalue()).decode()

        except Exception as e:
            logger.error(f"Failed to generate annotated screenshot: {e}")
            # Return original screenshot as fallback
            return base64.b64encode(screenshot_bytes).decode()

    def get_status(self) -> dict[str, Any]:
        """Get current analysis status."""
        return {
            "is_running": self._is_running,
            "analysis_id": self._current_analysis_id,
        }
