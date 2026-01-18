"""AI Test Generator Service.

Generates test code from natural language descriptions using AI.
Uses the runner's AI settings to connect to Claude CLI, Claude API, or Gemini.
"""

import json
import logging
from typing import Any

from .claude_cli_runner import clean_code_output, run_claude_cli

logger = logging.getLogger(__name__)


def build_test_generation_prompt(
    user_prompt: str,
    page_analysis: dict[str, Any] | None,
    test_type: str,
    multi_request_analysis: dict[str, Any] | None = None,
    collected_analyses: dict[str, Any] | None = None,
) -> str:
    """Build the full prompt for AI test generation.

    Args:
        user_prompt: User's natural language description of the test
        page_analysis: Page analysis data with detected elements
        test_type: Type of test to generate (playwright_cdp, qontinui_vision, etc.)
        multi_request_analysis: Optional multi-request ground truth data
        collected_analyses: Optional collected analyses (multiple types)

    Returns:
        Complete prompt string for the AI
    """
    lines: list[str] = []

    lines.append("## Task")
    lines.append(f"Generate a {test_type} test based on the following requirements.")
    lines.append("Return ONLY the test code, no explanations or markdown code blocks.")
    lines.append("")

    if page_analysis:
        elements = page_analysis.get("elements", [])
        if elements:
            lines.append("## Page Analysis (Ground Truth)")
            lines.append("")
            if test_type == "python_script":
                lines.append("**This is ground truth data from actual page analysis.**")
                lines.append("Use this to verify that API responses correctly identify these elements.")
                lines.append("Compare API-returned bounding boxes/elements against this reference.")
                lines.append("")
            lines.append("The following elements were detected on the page:")
            lines.append("")
            lines.append("| # | Label | Type | Text | Selector/Bounds |")
            lines.append("|---|-------|------|------|-----------------|")

            for idx, el in enumerate(elements[:30]):  # Limit to 30 elements
                label = el.get("label", "unknown")
                el_type = el.get("element_type", "unknown")
                text = el.get("text_content", "-")
                if text and len(text) > 30:
                    text = text[:30] + "..."
                text = f'"{text}"' if text and text != "-" else "-"

                selector = el.get("selector")
                if not selector:
                    box = el.get("bounding_box", {})
                    selector = f"({box.get('x', 0)}, {box.get('y', 0)})"

                lines.append(f"| {idx + 1} | {label} | {el_type} | {text} | {selector} |")

            lines.append("")

            if test_type == "python_script":
                lines.append("### Ground Truth Summary")
                lines.append(f"- Total elements: {len(elements)}")
                lines.append(f"- Element types: {', '.join(set(el.get('element_type', 'unknown') for el in elements))}")
                lines.append("")

        url = page_analysis.get("url")
        if url:
            lines.append(f"Page URL: {url}")
            lines.append("")

    # Format multi-request analysis (multiple API responses as ground truth)
    if multi_request_analysis:
        requests = multi_request_analysis.get("requests", [])
        successful_requests = [r for r in requests if r.get("status") == "complete"]

        if successful_requests:
            lines.append("## Multi-Request Ground Truth")
            lines.append("")
            lines.append("**Multiple API responses were collected as ground truth data.**")
            lines.append("Each response represents per-page extraction results that can be used for verification.")
            lines.append("")
            lines.append(f"**Total Requests:** {len(requests)} ({len(successful_requests)} successful)")
            if multi_request_analysis.get("total_elements"):
                lines.append(f"**Total Elements:** {multi_request_analysis['total_elements']}")
            lines.append("")

            for idx, req in enumerate(successful_requests):
                lines.append(f"### Request {idx + 1}: {req.get('name', 'Unnamed')}")
                lines.append(f"- **Method:** {req.get('method', 'GET')}")
                lines.append(f"- **Endpoint:** {req.get('endpoint', '')}")
                if req.get("duration_ms"):
                    lines.append(f"- **Duration:** {req['duration_ms']}ms")
                lines.append("")

                response = req.get("response")
                if response:
                    # Include full response as JSON for verification
                    lines.append("**Response Data:**")
                    lines.append("```json")
                    try:
                        response_str = json.dumps(response, indent=2)
                        # Truncate very long responses
                        if len(response_str) > 10000:
                            response_str = response_str[:10000] + "\n... (truncated)"
                        lines.append(response_str)
                    except (TypeError, ValueError):
                        lines.append(str(response)[:5000])
                    lines.append("```")
                    lines.append("")

            lines.append("### Verification Notes")
            lines.append("Use this multi-request data to:")
            lines.append("- Compare elements across different pages")
            lines.append("- Verify no duplicate elements appear in multiple pages/states")
            lines.append("- Check that bounding boxes are valid (positive dimensions, within screen bounds)")
            lines.append("- Validate that state groupings are logically consistent")
            lines.append("")

    # Format collected analyses (multiple analyses of different types)
    if collected_analyses:
        analyses = collected_analyses.get("analyses", [])
        if analyses:
            playwright_analyses = [a for a in analyses if a.get("type") == "playwright"]
            vision_analyses = [a for a in analyses if a.get("type") == "vision"]
            api_analyses = [a for a in analyses if a.get("type") == "api_request"]

            lines.append("## Collected Analyses (Ground Truth)")
            lines.append("")
            lines.append(f"**{len(analyses)} analyses collected:**")
            lines.append(f"- {len(playwright_analyses)} Playwright DOM analyses")
            lines.append(f"- {len(vision_analyses)} Vision captures")
            lines.append(f"- {len(api_analyses)} API request responses")
            lines.append("")

            # Format Playwright analyses
            for idx, analysis in enumerate(playwright_analyses):
                data = analysis.get("data", {})
                elements = data.get("elements", [])
                lines.append(f"### Playwright Analysis {idx + 1}: {analysis.get('name', 'Unnamed')}")
                lines.append(f"- Elements detected: {len(elements)}")
                if data.get("url"):
                    lines.append(f"- URL: {data['url']}")
                lines.append("")
                if elements:
                    lines.append("| # | Label | Type | Text |")
                    lines.append("|---|-------|------|------|")
                    for i, el in enumerate(elements[:15]):
                        text = el.get("text_content", "-") or "-"
                        if len(text) > 25:
                            text = text[:25] + "..."
                        lines.append(f"| {i + 1} | {el.get('label', '-')} | {el.get('element_type', '-')} | {text} |")
                    if len(elements) > 15:
                        lines.append(f"*... and {len(elements) - 15} more elements*")
                    lines.append("")

            # Format Vision analyses
            for idx, analysis in enumerate(vision_analyses):
                data = analysis.get("data", {})
                elements = data.get("elements", [])
                lines.append(f"### Vision Analysis {idx + 1}: {analysis.get('name', 'Unnamed')}")
                lines.append(f"- Elements detected: {len(elements)}")
                lines.append("")
                if elements:
                    lines.append("| # | Label | Type | Bounds |")
                    lines.append("|---|-------|------|--------|")
                    for i, el in enumerate(elements[:15]):
                        box = el.get("bounding_box", {})
                        bounds = f"({box.get('x', 0)}, {box.get('y', 0)}, {box.get('width', 0)}x{box.get('height', 0)})"
                        lines.append(f"| {i + 1} | {el.get('label', '-')} | {el.get('element_type', '-')} | {bounds} |")
                    if len(elements) > 15:
                        lines.append(f"*... and {len(elements) - 15} more elements*")
                    lines.append("")

            # Format API analyses
            for idx, analysis in enumerate(api_analyses):
                data = analysis.get("data", {})
                lines.append(f"### API Analysis {idx + 1}: {analysis.get('name', 'Unnamed')}")
                lines.append(f"- Method: {data.get('method', 'GET')}")
                lines.append(f"- URL: {data.get('url', '')}")
                if data.get("status_code"):
                    lines.append(f"- Status: {data['status_code']}")
                if data.get("duration_ms"):
                    lines.append(f"- Duration: {data['duration_ms']}ms")
                lines.append("")

                response = data.get("response")
                if response:
                    lines.append("**Response Data:**")
                    lines.append("```json")
                    try:
                        response_str = json.dumps(response, indent=2)
                        if len(response_str) > 8000:
                            response_str = response_str[:8000] + "\n... (truncated)"
                        lines.append(response_str)
                    except (TypeError, ValueError):
                        lines.append(str(response)[:4000])
                    lines.append("```")
                    lines.append("")

            lines.append("### Using Collected Analyses")
            lines.append("Use this ground truth data to:")
            lines.append("- Verify API responses match actual page content")
            lines.append("- Compare element counts and positions across sources")
            lines.append("- Validate that detected elements exist and are correctly identified")
            lines.append("- Check for consistency between different analysis methods")
            lines.append("")

    lines.append("## User Request")
    lines.append("")
    lines.append(user_prompt)
    lines.append("")

    lines.append("## Test Type Requirements")
    lines.append("")

    if test_type == "playwright_cdp":
        lines.append("Generate a Playwright test in TypeScript that:")
        lines.append("- Uses @playwright/test's test and expect")
        lines.append("- Assumes connection via CDP to existing browser")
        lines.append("- Uses proper selectors from the element analysis")
        lines.append("- Includes meaningful assertions")
    elif test_type == "qontinui_vision":
        lines.append("Generate a Qontinui Vision assertion config in JSON that:")
        lines.append("- Uses element bounding boxes from the analysis")
        lines.append("- Supports assertion types: element_present, text_present, element_within_bounds")
        lines.append("- Returns a valid JSON object with an 'assertions' array")
    elif test_type == "python_script":
        lines.append("Generate a Python test script that:")
        lines.append("- Uses assertion(condition, message) for assertions")
        lines.append("- Uses log(message) for logging")
        lines.append("- Uses metric(name, value) for metrics")
        lines.append("- Has a test_main() function as the entry point")
        lines.append("")
        lines.append("### Available Helpers")
        lines.append("```python")
        lines.append("def assertion(condition: bool, message: str) -> None:")
        lines.append("    '''Record an assertion. If condition is False, test fails.'''")
        lines.append("")
        lines.append("def log(message: str) -> None:")
        lines.append("    '''Log a message to the test output.'''")
        lines.append("")
        lines.append("def metric(name: str, value: float) -> None:")
        lines.append("    '''Record a numeric metric.'''")
        lines.append("")
        lines.append("# Environment variables available:")
        lines.append("# LAST_API_RESPONSE - JSON string of the last API response")
        lines.append("# WORKFLOW_CONFIG - JSON string of the linked workflow config")
        lines.append("# EXTRACTED_VARS - JSON dict of variables extracted from previous steps")
        lines.append("```")
        lines.append("")
        lines.append("### Output Format")
        lines.append("The script should print a JSON object at the end with this structure:")
        lines.append("```json")
        lines.append('{')
        lines.append('  "passed": true,')
        lines.append('  "summary": "Brief summary of verification result",')
        lines.append('  "issues": ["List of specific issues found"],')
        lines.append('  "metrics": {"metric_name": value}')
        lines.append('}')
        lines.append("```")
        lines.append("")
        if page_analysis:
            lines.append("### Using Ground Truth Data")
            lines.append("Page Analysis data is available. You can use it to:")
            lines.append("- Verify API-detected elements match actual page elements")
            lines.append("- Compare bounding boxes (API response vs ground truth)")
            lines.append("- Check if API missed any clickable elements")
            lines.append("- Validate that element counts are reasonable")
            lines.append("")
            lines.append("The ground truth data is provided above under 'Page Analysis (Ground Truth)'.")
            lines.append("Store it as a constant in your script and compare against API response.")
    elif test_type == "repository_test":
        lines.append("Generate a configuration comment with:")
        lines.append("- The test command to run")
        lines.append("- Expected output format")
        lines.append("- Any setup requirements")

    lines.append("")
    lines.append("Return only the test code, no explanation.")

    return "\n".join(lines)


def generate_test_via_claude_cli(
    prompt: str,
    timeout_seconds: int = 120,
    execution_mode: str = "auto",
    custom_path: str | None = None,
) -> dict[str, Any]:
    """Generate test code using Claude CLI.

    Args:
        prompt: The full prompt for test generation
        timeout_seconds: Maximum time to wait
        execution_mode: How to run Claude (auto, wsl, native)
        custom_path: Optional custom path to Claude executable

    Returns:
        Dict with success, code, and error fields
    """
    result = {"success": False, "code": "", "error": ""}

    cli_result = run_claude_cli(
        prompt=prompt,
        timeout_seconds=timeout_seconds,
        execution_mode=execution_mode,
        custom_path=custom_path,
    )

    if cli_result["success"]:
        result["success"] = True
        result["code"] = clean_code_output(cli_result["output"])
    else:
        result["error"] = cli_result["error"]

    return result


def generate_test_via_claude_api(
    prompt: str,
    api_key: str,
    model: str = "claude-sonnet-4-20250514",
    max_tokens: int = 4096,
) -> dict[str, Any]:
    """Generate test code using Claude API.

    Args:
        prompt: The full prompt for test generation
        api_key: Anthropic API key
        model: Model name to use
        max_tokens: Maximum tokens in response

    Returns:
        Dict with success, code, and error fields
    """
    result = {"success": False, "code": "", "error": ""}

    try:
        import httpx

        response = httpx.post(
            "https://api.anthropic.com/v1/messages",
            headers={
                "x-api-key": api_key,
                "anthropic-version": "2023-06-01",
                "content-type": "application/json",
            },
            json={
                "model": model,
                "max_tokens": max_tokens,
                "messages": [{"role": "user", "content": prompt}],
            },
            timeout=120.0,
        )

        if response.status_code == 200:
            data = response.json()
            if data.get("content") and len(data["content"]) > 0:
                code = data["content"][0].get("text", "")
                code = _clean_code_output(code)
                result["success"] = True
                result["code"] = code
            else:
                result["error"] = "Empty response from Claude API"
        else:
            result["error"] = f"Claude API error {response.status_code}: {response.text}"

    except ImportError:
        result["error"] = "httpx not installed. Run: pip install httpx"
    except Exception as e:
        result["error"] = f"Claude API request failed: {e}"

    return result




class AiTestGeneratorService:
    """Service for generating tests using AI."""

    def __init__(self, event_manager=None):
        """Initialize the service.

        Args:
            event_manager: Optional event manager for logging
        """
        self.event_manager = event_manager

    def _log(self, level: str, message: str):
        """Log a message."""
        if self.event_manager:
            self.event_manager.emit_log(level, message)
        else:
            logger.log(getattr(logging, level.upper(), logging.INFO), message)

    def generate_test(
        self,
        user_prompt: str,
        test_type: str,
        page_analysis: dict[str, Any] | None = None,
        multi_request_analysis: dict[str, Any] | None = None,
        collected_analyses: dict[str, Any] | None = None,
        ai_provider: str = "claude_cli",
        ai_settings: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Generate a test using AI.

        Args:
            user_prompt: User's description of the test
            test_type: Type of test (playwright_cdp, qontinui_vision, python_script, repository_test)
            page_analysis: Optional page analysis data
            multi_request_analysis: Optional multi-request ground truth data
            collected_analyses: Optional collected analyses (multiple types)
            ai_provider: AI provider to use (claude_cli, claude_api, gemini_cli, gemini_api)
            ai_settings: Provider-specific settings

        Returns:
            Dict with success, code, and error fields
        """
        self._log("info", f"Generating {test_type} test using {ai_provider}")

        # Build the prompt
        full_prompt = build_test_generation_prompt(
            user_prompt, page_analysis, test_type, multi_request_analysis, collected_analyses
        )
        self._log("debug", f"Built prompt with {len(full_prompt)} characters")

        # Generate based on provider
        ai_settings = ai_settings or {}

        if ai_provider == "claude_cli":
            execution_mode = ai_settings.get("execution_mode", "auto")
            timeout = ai_settings.get("timeout_seconds", 120)
            custom_path = ai_settings.get("custom_path")
            result = generate_test_via_claude_cli(full_prompt, timeout, execution_mode, custom_path)

        elif ai_provider == "claude_api":
            api_key = ai_settings.get("api_key", "")
            if not api_key:
                return {"success": False, "code": "", "error": "Claude API key not configured"}
            model = ai_settings.get("model", "claude-sonnet-4-20250514")
            max_tokens = ai_settings.get("max_tokens", 4096)
            result = generate_test_via_claude_api(full_prompt, api_key, model, max_tokens)

        elif ai_provider in ("gemini_cli", "gemini_api"):
            # TODO: Implement Gemini support
            return {"success": False, "code": "", "error": f"{ai_provider} not yet implemented for test generation"}

        else:
            return {"success": False, "code": "", "error": f"Unknown AI provider: {ai_provider}"}

        if result["success"]:
            self._log("info", f"Successfully generated {len(result['code'])} characters of test code")
        else:
            self._log("error", f"Test generation failed: {result['error']}")

        return result
