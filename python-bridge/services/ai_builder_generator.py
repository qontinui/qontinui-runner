"""AI Builder Generator Service.

Generates content for builder tabs using AI:
- Context (knowledge base entries)
- API Request templates
- Task prompts
- Exploration strategies
"""

import json
import logging
import re
from typing import Any

from .claude_cli_runner import run_claude_cli

logger = logging.getLogger(__name__)


# =============================================================================
# Prompt Builders
# =============================================================================


def build_context_prompt(user_prompt: str) -> str:
    """Build prompt for context/knowledge base generation."""
    return f"""## Task
Generate a knowledge base context entry for an AI automation system.

## User Request
{user_prompt}

## Requirements
Create a knowledge base entry that will help AI assistants perform automation tasks.
The entry should include:
1. A clear, descriptive name
2. Well-structured Markdown content (2-4 paragraphs)
3. Relevant tags for categorization
4. Keywords that should trigger auto-inclusion of this context

## Output Format
Return a JSON object with exactly these fields:
```json
{{
  "name": "Context Title",
  "content": "Markdown content with helpful information...\\n\\nUse multiple paragraphs for clarity.",
  "category": "suggested_category",
  "tags": ["tag1", "tag2", "tag3"],
  "taskMentions": ["keyword1", "keyword2"]
}}
```

- `name`: Descriptive title (5-10 words)
- `content`: Markdown documentation (200-500 words)
- `category`: Single category like "debugging", "testing", "architecture"
- `tags`: 3-5 relevant tags
- `taskMentions`: 2-3 keywords that should trigger this context

Return ONLY the JSON object, no explanations or markdown code blocks."""


def build_api_request_prompt(user_prompt: str, base_url: str | None = None) -> str:
    """Build prompt for API request template generation."""
    url_context = ""
    if base_url:
        url_context = f"\n## Base URL Context\nUse this as the base: {base_url}\n"

    return f"""## Task
Generate an API request template based on the following description.

## User Request
{user_prompt}
{url_context}
## Requirements
Create a complete HTTP request template with:
1. Appropriate HTTP method (GET, POST, PUT, PATCH, DELETE)
2. URL with placeholders for dynamic parts (use {{id}}, {{name}}, etc.)
3. Required headers (Content-Type, Accept, Authorization, etc.)
4. Request body (if applicable) with example values
5. Reasonable timeout setting

## Output Format
Return a JSON object with exactly these fields:
```json
{{
  "name": "Request Name",
  "description": "Brief description of what this request does",
  "method": "POST",
  "url": "https://api.example.com/users/{{id}}",
  "headers": {{
    "Content-Type": "application/json",
    "Accept": "application/json"
  }},
  "body": "{{\\n  \\"name\\": \\"John Doe\\",\\n  \\"email\\": \\"john@example.com\\"\\n}}",
  "body_content_type": "application/json",
  "timeout_ms": 30000
}}
```

- `name`: Short name for the request (2-5 words)
- `description`: What this request does (1 sentence)
- `method`: HTTP method (GET, POST, PUT, PATCH, DELETE)
- `url`: Full URL with placeholders
- `headers`: Required headers as key-value object
- `body`: Request body as JSON string (omit for GET/DELETE without body)
- `body_content_type`: "application/json", "application/x-www-form-urlencoded", "text/plain", or "none"
- `timeout_ms`: Suggested timeout in milliseconds

Return ONLY the JSON object, no explanations or markdown code blocks."""


def build_task_prompt(user_prompt: str, mode: str = "generate") -> str:
    """Build prompt for task/prompt generation or improvement."""
    if mode == "improve":
        return f"""## Task
Improve the following AI task prompt to make it clearer, more structured, and more effective.

## Original Prompt
{user_prompt}

## Requirements
Analyze the prompt and improve it by:
1. Making instructions clearer and more specific
2. Adding structure (numbered steps, sections)
3. Specifying expected output format
4. Adding relevant constraints or guidelines
5. Removing ambiguity

## Output Format
Return a JSON object with exactly these fields:
```json
{{
  "name": "Improved Task Name",
  "description": "What this task does",
  "content": "The improved prompt content...\\n\\nWith clear structure and instructions.",
  "category": "suggested_category",
  "tags": ["tag1", "tag2"]
}}
```

- `name`: Descriptive name for the task (3-6 words)
- `description`: Brief description (1 sentence)
- `content`: The improved prompt (keep it focused and actionable)
- `category`: Category like "automation", "testing", "analysis", "documentation"
- `tags`: 2-4 relevant tags

Return ONLY the JSON object, no explanations or markdown code blocks."""
    else:
        return f"""## Task
Generate an AI task prompt for the following use case.

## User Request
{user_prompt}

## Requirements
Create a well-structured prompt that:
1. Clearly states the objective
2. Provides necessary context
3. Specifies the expected output format
4. Includes important constraints
5. Is optimized for AI understanding

## Output Format
Return a JSON object with exactly these fields:
```json
{{
  "name": "Task Name",
  "description": "What this task accomplishes",
  "content": "Your task is to...\\n\\n## Requirements\\n1. First requirement\\n2. Second requirement\\n\\n## Output Format\\nProvide your response as...",
  "category": "suggested_category",
  "tags": ["tag1", "tag2"]
}}
```

- `name`: Descriptive name for the task (3-6 words)
- `description`: Brief description (1 sentence)
- `content`: The full prompt content with clear structure
- `category`: Category like "automation", "testing", "analysis", "documentation"
- `tags`: 2-4 relevant tags

Return ONLY the JSON object, no explanations or markdown code blocks."""


def build_exploration_prompt(
    user_goal: str,
    available_states: list[dict],
    available_transitions: list[dict],
) -> str:
    """Build prompt for exploration strategy suggestion."""
    states_text = ""
    if available_states:
        state_lines = []
        for s in available_states[:20]:  # Limit to avoid token overflow
            flags = []
            if s.get("is_initial"):
                flags.append("initial")
            if s.get("is_final"):
                flags.append("final")
            flag_str = f" ({', '.join(flags)})" if flags else ""
            desc = f": {s['description']}" if s.get("description") else ""
            state_lines.append(f"- {s['id']}: {s['name']}{flag_str}{desc}")
        states_text = "\n## Available States\n" + "\n".join(state_lines)

    transitions_text = ""
    if available_transitions:
        trans_lines = []
        for t in available_transitions[:30]:  # Limit to avoid token overflow
            from_to = ""
            if t.get("from_state") or t.get("to_state"):
                from_to = f" ({t.get('from_state', '?')} -> {t.get('to_state', '?')})"
            trans_lines.append(f"- {t['id']}: {t['name']}{from_to}")
        transitions_text = "\n## Available Transitions\n" + "\n".join(trans_lines)

    return f"""## Task
Suggest an exploration strategy for a state machine based on the user's goal.

## User Goal
{user_goal}
{states_text}
{transitions_text}

## Available Strategies
1. **smoke_test**: Quick verification of critical paths only
2. **exhaustive**: Verify all states and transitions (thorough but slow)
3. **regression**: Focus on previously failed areas
4. **random_walk**: Random exploration for discovering unexpected issues
5. **targeted**: Focus on specific states/transitions (requires target_state_ids or target_transition_ids)

## Requirements
Based on the user's goal and available states/transitions:
1. Recommend the best strategy
2. If "targeted" is best, specify which states/transitions to focus on
3. Suggest appropriate limits (max_states, max_duration)
4. Explain your reasoning

## Output Format
Return a JSON object with exactly these fields:
```json
{{
  "strategy": "targeted",
  "target_state_ids": ["state1", "state2"],
  "target_transition_ids": [],
  "max_states": 10,
  "max_duration_seconds": 300,
  "rationale": "Brief explanation of why this strategy is recommended..."
}}
```

- `strategy`: One of "smoke_test", "exhaustive", "regression", "random_walk", "targeted"
- `target_state_ids`: Array of state IDs to focus on (only for "targeted" strategy)
- `target_transition_ids`: Array of transition IDs to focus on (only for "targeted" strategy)
- `max_states`: Recommended limit (0 for unlimited)
- `max_duration_seconds`: Recommended duration limit (0 for unlimited)
- `rationale`: 1-2 sentence explanation

Return ONLY the JSON object, no explanations or markdown code blocks."""


def _extract_valid_selectors(elements: list[dict] | None) -> list[str]:
    """Extract all valid selectors from elements list."""
    if not elements:
        return []
    selectors = []
    for el in elements:
        el_id = el.get("id", "")
        if el_id:
            selectors.append(f'[data-ui-id="{el_id}"]')
    return selectors


def _extract_valid_ids(elements: list[dict] | None) -> set[str]:
    """Extract all valid data-ui-id values from elements list."""
    if not elements:
        return set()
    ids = set()
    for el in elements:
        el_id = el.get("id", "")
        if el_id:
            ids.add(el_id)
    return ids


def _validate_test_selectors(test_code: str, valid_ids: set[str]) -> tuple[bool, list[str]]:
    """Validate that test code only uses valid data-ui-id selectors.

    Returns:
        tuple: (is_valid, list of invalid selectors found)
    """
    # Find all data-ui-id selectors in the test code
    # Pattern matches [data-ui-id="..."] with various quote styles
    pattern = r'\[data-ui-id=["\']([^"\']+)["\']\]'
    found_ids = re.findall(pattern, test_code)

    logger.info(f"[SELECTOR VALIDATION] Found {len(found_ids)} data-ui-id selectors in test code")
    logger.info(f"[SELECTOR VALIDATION] All found IDs: {found_ids}")

    invalid_ids = []
    for found_id in found_ids:
        if found_id not in valid_ids:
            invalid_ids.append(found_id)
            logger.info(f"[SELECTOR VALIDATION] INVALID ID: '{found_id}'")
        else:
            logger.info(f"[SELECTOR VALIDATION] Valid ID: '{found_id}'")

    return len(invalid_ids) == 0, invalid_ids


def _format_elements_list(elements: list[dict] | None) -> str:
    """Format a list of elements into a readable string for AI prompt.

    Elements from UI Bridge have data-ui-id attributes, not native id attributes.
    The selector should be [data-ui-id="..."] not #id.
    """
    if not elements:
        return "(no elements)"

    element_lines = []
    for el in elements:
        el_type = el.get("type", "")
        el_text = el.get("text", "") or el.get("label", "")
        el_id = el.get("id", "")
        tag_name = el.get("tagName", "element")
        visible = "visible" if el.get("visible", True) else "hidden"
        enabled = "enabled" if el.get("enabled", True) else "disabled"

        # Build a detailed element description with the EXACT selector to use
        parts = []
        if el_id:
            # Make the Playwright selector very obvious - this is what the AI MUST use
            parts.append(f'  - **USE THIS SELECTOR**: `[data-ui-id="{el_id}"]`')
            parts.append(f'    `<{tag_name}>` type="{el_type}"')
            if el_text:
                parts.append(f'    text="{el_text[:100]}"')
            parts.append(f"    ({visible}, {enabled})")
        else:
            parts.append(f"  - `<{tag_name}>` (no data-ui-id)")
            if el_type:
                parts.append(f'type="{el_type}"')
            if el_text:
                parts.append(f'text="{el_text[:100]}"')
            parts.append(f"({visible}, {enabled})")
        element_lines.append(" ".join(parts) if not el_id else "\n".join(parts))

    return "\n".join(element_lines)


def build_test_and_agentic_prompt(
    user_prompt: str,
    page_context: dict | None = None,
    contexts_content: str | None = None,
) -> str:
    """Build prompt for test and agentic step generation."""
    context_section = ""
    selector_instructions = ""
    knowledge_context = ""

    # Include knowledge contexts if provided
    if contexts_content:
        knowledge_context = f"\n{contexts_content}\n"

    if page_context:
        # Check if this is multi-page context (has 'pages' array) or single page
        pages = page_context.get("pages")

        if pages and isinstance(pages, list) and len(pages) > 0:
            # Multi-page context from flow capture
            total_elements = sum(len(p.get("elements") or []) for p in pages)

            # Collect all valid selectors for quick reference
            all_selectors = []
            pages_text_parts = []
            for i, page in enumerate(pages, 1):
                page_url = page.get("url", "Unknown URL")
                page_title = page.get("title", "Unknown Title")
                page_elements = page.get("elements") or []
                elements_text = _format_elements_list(page_elements)
                all_selectors.extend(_extract_valid_selectors(page_elements))

                page_section = f"""### Page {i}: {page_title}
- **URL**: {page_url}
- **Elements** ({len(page_elements)} total):
{elements_text}
"""
                pages_text_parts.append(page_section)

            # Extract raw IDs for UI Bridge API
            all_ids = []
            for page in pages:
                all_ids.extend([el.get("id") for el in page.get("elements", []) if el.get("id")])
            id_cheat_sheet = "\n".join([f'  "{el_id}"' for el_id in sorted(set(all_ids))])

            context_section = f"""
## Multi-Page Context (from UI Bridge Flow Capture - REAL DOM)
This test involves navigation across {len(pages)} page(s) with {total_elements} total elements.

{"".join(pages_text_parts)}

## VALID UI BRIDGE ELEMENT IDs - USE THESE EXACTLY
These are the element IDs captured from the UI Bridge. Use them directly with the API:
{id_cheat_sheet}
"""
            selector_instructions = f"""
**CRITICAL: This is a MULTI-PAGE test. You MUST use ONLY the element IDs from the "VALID UI BRIDGE ELEMENT IDs" list above.**

UI BRIDGE API USAGE:
1. Use element IDs directly with the UI Bridge API (NOT CSS selectors)
2. Example: `click_element("my-button-id")` - use the ACTUAL ID string from the list above
3. Example: `get_element_state("my-input-id")` - use the ACTUAL ID string from the list above
4. COPY the exact ID string from the list - do NOT modify, abbreviate, or invent IDs
5. The test navigates through {len(pages)} page(s) - use elements from the correct page
6. If an element you need is not in the list, find the closest match from the list
7. NEVER invent or guess IDs like "my-tab", "submit-btn", or "main-content" - always use IDs from the actual list above
"""
        else:
            # Single page context
            url = page_context.get("url", "Unknown URL")
            title = page_context.get("title", "Unknown Title")
            elements = page_context.get("elements") or []
            elements_text = _format_elements_list(elements)
            # Extract raw IDs for UI Bridge API
            valid_ids = [el.get("id") for el in elements if el.get("id")]
            id_cheat_sheet = "\n".join([f'  "{el_id}"' for el_id in sorted(set(valid_ids))])

            context_section = f"""
## Current Page Context (from UI Bridge - REAL DOM)
- **URL**: {url}
- **Title**: {title}
- **Elements discovered** ({len(elements)} total):
{elements_text}

## VALID UI BRIDGE ELEMENT IDs - USE THESE EXACTLY
These are the element IDs captured from the UI Bridge. Use them directly with the API:
{id_cheat_sheet}
"""
            selector_instructions = """
**CRITICAL: You MUST use ONLY the element IDs from the "VALID UI BRIDGE ELEMENT IDs" list above.**

UI BRIDGE API USAGE:
1. Use element IDs directly with the UI Bridge API (NOT CSS selectors)
2. Example: `click_element("my-button-id")` - use the ACTUAL ID string from the list above
3. Example: `get_element_state("my-input-id")` - use the ACTUAL ID string from the list above
4. COPY the exact ID string from the list - do NOT modify, abbreviate, or invent IDs
5. If the needed element isn't in the list, find the closest match from the list
6. NEVER use placeholder IDs like "extraction-config-tab" - always use IDs from the actual list
"""
    else:
        context_section = """
## No Page Context Available
No UI Bridge connection - generating generic test structure.
You will need to replace placeholder selectors with actual element selectors.
"""

    return f"""## Task
Generate BOTH a Python verification test AND an agentic step prompt based on the user's instructions.

## User Instructions
{user_prompt}
{knowledge_context}{context_section}
## Requirements

### 1. Verification Test (Python)
Create a Python script that uses the UI Bridge HTTP API to automate the browser.

**IMPORTANT: The script runs in a special wrapper with these helper functions:**
- `assertion(name, passed, message=None, expected=None, actual=None)` - Record test result
- `log(msg)` - Add a log message to output
- `fail(message)` - Mark the test as failed with a message

**The script must:**
- Use the UI Bridge HTTP API on http://localhost:9876 to interact with browser elements
- Use data-ui-id selectors to identify elements (these are the element IDs in UI Bridge)
- Use `assertion()` calls instead of Python `assert` statements
- Include appropriate waits for dynamic content
- Use descriptive variable names

**UI Bridge HTTP API (via Browser Extension):**
The test uses the `/extension/command` endpoint to send commands to the browser extension.
The extension must be connected to the runner and have a tab selected for automation.

- Base URL: http://localhost:9876
- GET /extension/status - Check if extension is connected
- POST /extension/command - Send commands to the extension

**Extension Commands:**
- `{{"action": "getElements", "params": {{}}}}` - Get all elements with data-ui-id
- `{{"action": "executeAction", "params": {{"elementId": "...", "action": "click", "params": {{}}}}}}` - Click element
- `{{"action": "selectTab", "params": {{"tabId": ...}}}}` - Select a browser tab
- `{{"action": "listTabs", "params": {{}}}}` - List available browser tabs

**IMPORTANT: Extension Connection Required**
The browser extension must be:
1. Installed in Chrome/Chromium browser
2. Connected via WebSocket (check with GET /extension/status)
3. Have a tab selected (via selectTab or the extension popup)

If `get_elements()` returns 0 elements, the extension is NOT connected.

**Template structure:**
```python
import requests
import time

BASE_URL = "http://localhost:9876"

def check_extension():
    resp = requests.get(f"{{BASE_URL}}/extension/status")
    data = resp.json()
    return data.get("data", {{}}).get("connected", False)

def send_command(action, params=None):
    resp = requests.post(
        f"{{BASE_URL}}/extension/command",
        json={{"action": action, "params": params or {{}}}},
        timeout=30
    )
    result = resp.json()
    if not result.get("success"):
        raise Exception(result.get("error", f"Command {{action}} failed"))
    return result.get("data", {{}})

def get_elements():
    data = send_command("getElements")
    return data.get("elements", [])

def click_element(element_id):
    return send_command("executeAction", {{
        "elementId": element_id,
        "action": "click",
        "params": {{}}
    }})

def type_text(element_id, text):
    return send_command("executeAction", {{
        "elementId": element_id,
        "action": "type",
        "params": {{"text": text}}
    }})

def wait_for_element(element_id, timeout=30):
    start = time.time()
    while time.time() - start < timeout:
        try:
            elements = get_elements()
            for el in elements:
                if el.get("id") == element_id and el.get("visible"):
                    return True
        except:
            pass
        time.sleep(0.5)
    return False

try:
    # Check extension connection
    if not check_extension():
        fail("Browser extension not connected. Please ensure the Qontinui DevTools extension is installed and connected.")

    log("Extension connected, checking elements...")

    # Get all elements
    elements = get_elements()
    log(f"Found {{len(elements)}} elements")

    if len(elements) == 0:
        fail("No elements found. Make sure a browser tab is selected in the extension popup.")

    # ================================================================
    # IMPORTANT: Replace the placeholder IDs below with ACTUAL element
    # IDs from the "VALID UI BRIDGE ELEMENT IDs" list above.
    # Do NOT use these example IDs directly - they are placeholders!
    # ================================================================

    # Example: Click on a tab element (REPLACE with actual element ID)
    # click_element("YOUR_TAB_ELEMENT_ID_HERE")
    # time.sleep(1)

    # Example: Click a button (REPLACE with actual element ID)
    # click_element("YOUR_BUTTON_ELEMENT_ID_HERE")
    # log("Clicked button")

    # Example: Wait for a result element (REPLACE with actual element ID)
    # found = wait_for_element("YOUR_RESULT_ELEMENT_ID_HERE", timeout=30)
    # assertion("results_appeared", found, "Results should appear")

    # Example: Count elements matching a pattern
    # elements = get_elements()
    # matching_elements = [e for e in elements if e.get("id", "").startswith("your-prefix-")]
    # count = len(matching_elements)
    # assertion("element_count", count > 0, f"Expected elements, got {{count}}", expected=">0", actual=count)

    # YOUR TEST CODE HERE - use actual element IDs from the list above
    assertion("test_implemented", False, "Replace this placeholder with actual test logic using element IDs from the list above")

except Exception as e:
    fail(f"Test failed: {{e}}")
```
{selector_instructions}
### 2. Agentic Step (AI Prompt)
Create a prompt for an AI agent that MAKES CHANGES or FIXES ISSUES, NOT for running tests.

**CRITICAL: THE VERIFICATION TEST USES UI BRIDGE, NOT PLAYWRIGHT**

The agentic step prompt MUST include this context at the top:
```
## Test Technology
This verification test uses the **UI Bridge HTTP API** (NOT Playwright).
- The test calls HTTP endpoints on localhost:9876 to interact with browser elements
- UI Bridge requires the browser extension to be connected
- "Found 0 elements" means the UI Bridge extension is not connected to a browser tab
- This is NOT a Playwright test - do not debug as if it were Playwright
```

**CRITICAL DISTINCTION:**
- The **Verification Test** (above) is for TESTING and VALIDATING via UI Bridge HTTP API
- The **Agentic Step** is for MAKING CHANGES to fix issues - it should NOT run tests

The agentic step prompt should:
- ALWAYS mention this is a UI Bridge test, NOT Playwright
- Describe what CODE CHANGES or FIXES the AI should make
- Reference specific files, functions, or components to modify
- Explain what bug or issue needs to be fixed
- Provide context about the expected behavior after the fix
- Guide the AI to implement a solution, NOT to run tests or click buttons

**DO NOT include in the agentic step:**
- Instructions to run tests (that's what the verification test is for)
- Instructions to click buttons or interact with UI (that's for automated tests)
- Instructions to verify or assert outcomes (that's what the verification test does)
- Instructions to wait for results (that's what the verification test does)
- References to Playwright - this test does NOT use Playwright

**GOOD agentic step example:**
"## Test Technology
This test uses UI Bridge HTTP API (NOT Playwright). If 'Found 0 elements' appears, the browser extension is not connected.

## Task
Fix the state extraction logic in qontinui/extraction.py. The current implementation only identifies 3 states because [reason]. Modify the clustering algorithm to correctly group co-occurring images into separate states."

**BAD agentic step example (DO NOT generate this):**
"The Playwright test is failing because the page isn't loading. Check the selectors..."

## Output Format
Return a JSON object with exactly these fields:
```json
{{
  "verification_test": "import requests\\nimport time\\n\\nBASE_URL = 'http://localhost:9876'\\n\\ndef send_command(action, params=None):\\n    resp = requests.post(f'{{BASE_URL}}/extension/command', json={{'action': action, 'params': params or {{}}}}, timeout=30)\\n    result = resp.json()\\n    if not result.get('success'): raise Exception(result.get('error', 'Command failed'))\\n    return result.get('data', {{}})\\n\\ndef click_element(elem_id):\\n    return send_command('executeAction', {{'elementId': elem_id, 'action': 'click', 'params': {{}}}})\\n\\n# IMPORTANT: Replace element IDs below with ACTUAL IDs from the page analysis!\\ntry:\\n    # Use actual element IDs from the list provided above\\n    click_element('ACTUAL_ELEMENT_ID_FROM_LIST')\\n    time.sleep(1)\\n    # ... add your test logic using real element IDs\\n    assertion('test_completed', True, 'Test completed successfully')\\nexcept Exception as e:\\n    fail(f'Test failed: {{e}}')",
  "agentic_step": "## Test Technology\\nThis test uses UI Bridge HTTP API via browser extension (NOT Playwright). If 'Found 0 elements' appears, check extension connection.\\n\\n## Task\\nFix the [specific component] in [file path].\\n\\n## Problem\\nDescribe the bug or issue that needs code changes.\\n\\n## Solution\\nExplain what code modifications to make.\\n\\n## Files to Modify\\n- path/to/file.py: Description of changes",
  "test_name": "test_descriptive_name",
  "agentic_name": "Fix Component Bug"
}}
```

- `verification_test`: Python script using UI Bridge HTTP API with `assertion()` helper calls
- `agentic_step`: Prompt for an AI to MAKE CODE CHANGES - NOT for running tests or UI interactions
- `test_name`: Descriptive name for the test (snake_case, starts with test_)
- `agentic_name`: Human-readable name describing what CODE CHANGE to make (e.g., "Fix Extraction Algorithm")

**CRITICAL - Test Format Rules:**
- DO NOT use `import pytest` or Playwright
- DO NOT use pytest-style function definitions
- DO use `import requests` and call UI Bridge HTTP API on localhost:9876
- DO use `assertion(name, passed, message)` instead of Python `assert` statements
- DO use element IDs from the data-ui-id attributes (these are the UI Bridge element IDs)
- The test runs as a standalone script, not under pytest

**Remember:**
- Verification test = calls UI Bridge API to click buttons, uses assertion() helper (runs in verification phase)
- Agentic step = describes what code to fix/change (runs in agentic phase, AI writes code)

Return ONLY the JSON object, no explanations or markdown code blocks."""


# =============================================================================
# AI Provider Functions
# =============================================================================


def generate_via_claude_cli(
    prompt: str,
    timeout_seconds: int = 120,
    execution_mode: str = "auto",
    custom_path: str | None = None,
) -> dict[str, Any]:
    """Generate content using Claude CLI."""
    import tempfile

    logger.debug(f"[generate_via_claude_cli] Starting, prompt_len={len(prompt)}")
    result = {"success": False, "data": None, "error": ""}

    # Run from temp directory to avoid CLAUDE.md context
    temp_dir = tempfile.gettempdir()

    cli_result = run_claude_cli(
        prompt=prompt,
        timeout_seconds=timeout_seconds,
        execution_mode=execution_mode,
        custom_path=custom_path,
        working_directory=temp_dir,
    )

    logger.debug(
        f"[generate_via_claude_cli] CLI result: success={cli_result['success']}, output_len={len(cli_result.get('output', ''))}"
    )

    if cli_result["success"]:
        parsed = _parse_json_output(cli_result["output"])
        logger.debug(f"[generate_via_claude_cli] Parsed result: success={parsed['success']}")
        if parsed["success"]:
            result["success"] = True
            result["data"] = parsed["data"]
            logger.debug(
                f"[generate_via_claude_cli] Data keys: {list(parsed['data'].keys()) if isinstance(parsed['data'], dict) else type(parsed['data'])}"
            )
        else:
            result["error"] = parsed["error"]
            logger.error(f"[generate_via_claude_cli] Parse error: {parsed['error']}")
    else:
        result["error"] = cli_result["error"]
        logger.error(f"[generate_via_claude_cli] CLI error: {cli_result['error']}")

    logger.debug(f"[generate_via_claude_cli] Returning: success={result['success']}")
    return result


def generate_via_claude_api(
    prompt: str,
    api_key: str,
    model: str = "claude-sonnet-4-20250514",
    max_tokens: int = 2048,
) -> dict[str, Any]:
    """Generate content using Claude API."""
    result = {"success": False, "data": None, "error": ""}

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
                output = data["content"][0].get("text", "")
                parsed = _parse_json_output(output)
                if parsed["success"]:
                    result["success"] = True
                    result["data"] = parsed["data"]
                else:
                    result["error"] = parsed["error"]
            else:
                result["error"] = "Empty response from Claude API"
        else:
            result["error"] = f"Claude API error {response.status_code}: {response.text}"

    except ImportError:
        result["error"] = "httpx not installed. Run: pip install httpx"
    except Exception as e:
        result["error"] = f"Claude API request failed: {e}"

    return result


def _parse_json_output(output: str) -> dict[str, Any]:
    """Parse AI output as JSON."""
    logger.debug(f"[_parse_json_output] Input length: {len(output)}")
    logger.debug(f"[_parse_json_output] First 300 chars: {output[:300]!r}")

    output = output.strip()

    # Extract JSON from markdown code blocks (handles preamble text before the block)
    import re

    code_block_match = re.search(r"```(?:json)?\s*\n(.*?)```", output, re.DOTALL)
    if code_block_match:
        output = code_block_match.group(1).strip()
        logger.debug(f"[_parse_json_output] Extracted from code block, length: {len(output)}")
        logger.debug(f"[_parse_json_output] Extracted content, first 300 chars: {output[:300]!r}")
    elif output.startswith("```"):
        # Fallback: opening ``` without closing (shouldn't happen but handle gracefully)
        lines = output.split("\n")
        lines = lines[1:]
        if lines and lines[-1].strip() == "```":
            lines = lines[:-1]
        output = "\n".join(lines).strip()
        logger.debug(f"[_parse_json_output] After removing code blocks, length: {len(output)}")
    elif not output.startswith("{") and not output.startswith("["):
        # Try to find JSON object or array in the output (skip preamble text)
        json_start = -1
        for i, ch in enumerate(output):
            if ch in "{[":
                json_start = i
                break
        if json_start > 0:
            output = output[json_start:]
            logger.debug(f"[_parse_json_output] Skipped preamble, extracted from char {json_start}")

    try:
        data = json.loads(output)
        logger.debug(
            f"[_parse_json_output] JSON parsed successfully, keys: {list(data.keys()) if isinstance(data, dict) else type(data)}"
        )
        if isinstance(data, dict):
            # Log presence and length of key fields
            for key in ["verification_test", "agentic_step", "test_name", "agentic_name"]:
                if key in data:
                    val = data[key]
                    if isinstance(val, str):
                        logger.debug(
                            f"[_parse_json_output] {key}: length={len(val)}, first 100 chars: {val[:100]!r}"
                        )
                    else:
                        logger.debug(f"[_parse_json_output] {key}: {val!r}")
                else:
                    logger.debug(f"[_parse_json_output] {key}: MISSING")
        return {"success": True, "data": data, "error": ""}
    except json.JSONDecodeError as e:
        logger.error(f"[_parse_json_output] JSON decode error: {e}")
        logger.error(f"[_parse_json_output] Raw output (first 1000 chars): {output[:1000]!r}")
        return {
            "success": False,
            "data": None,
            "error": f"Failed to parse AI response as JSON: {e}\nRaw output: {output[:500]}",
        }


# =============================================================================
# Service Class
# =============================================================================


class AiBuilderGeneratorService:
    """Service for generating content for builder tabs using AI."""

    def __init__(self, event_manager=None):
        """Initialize the service."""
        self.event_manager = event_manager

    def _log(self, level: str, message: str):
        """Log a message."""
        if self.event_manager:
            self.event_manager.emit_log(level, message)
        else:
            logger.log(getattr(logging, level.upper(), logging.INFO), message)

    def _generate(
        self,
        prompt: str,
        ai_provider: str,
        ai_settings: dict[str, Any],
    ) -> dict[str, Any]:
        """Generate content using the specified AI provider."""
        if ai_provider == "claude_cli":
            return generate_via_claude_cli(
                prompt,
                timeout_seconds=ai_settings.get("timeout_seconds", 120),
                execution_mode=ai_settings.get("execution_mode", "auto"),
                custom_path=ai_settings.get("custom_path"),
            )
        elif ai_provider == "claude_api":
            api_key = ai_settings.get("api_key", "")
            if not api_key:
                return {"success": False, "data": None, "error": "Claude API key not configured"}
            return generate_via_claude_api(
                prompt,
                api_key=api_key,
                model=ai_settings.get("model", "claude-sonnet-4-20250514"),
                max_tokens=ai_settings.get("max_tokens", 2048),
            )
        else:
            return {
                "success": False,
                "data": None,
                "error": f"{ai_provider} not yet implemented for builder generation",
            }

    def generate_context(
        self,
        user_prompt: str,
        ai_provider: str = "claude_cli",
        ai_settings: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Generate a knowledge base context entry."""
        self._log("info", f"Generating context using {ai_provider}")

        prompt = build_context_prompt(user_prompt)
        result = self._generate(prompt, ai_provider, ai_settings or {})

        if result["success"]:
            self._log(
                "info", f"Successfully generated context: {result['data'].get('name', 'Unknown')}"
            )
        else:
            self._log("error", f"Context generation failed: {result['error']}")

        return result

    def generate_api_request(
        self,
        user_prompt: str,
        base_url: str | None = None,
        ai_provider: str = "claude_cli",
        ai_settings: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Generate an API request template."""
        self._log("info", f"Generating API request using {ai_provider}")

        prompt = build_api_request_prompt(user_prompt, base_url)
        result = self._generate(prompt, ai_provider, ai_settings or {})

        if result["success"]:
            self._log(
                "info",
                f"Successfully generated API request: {result['data'].get('name', 'Unknown')}",
            )
        else:
            self._log("error", f"API request generation failed: {result['error']}")

        return result

    def generate_task_prompt(
        self,
        user_prompt: str,
        mode: str = "generate",
        ai_provider: str = "claude_cli",
        ai_settings: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Generate or improve a task prompt."""
        self._log(
            "info",
            f"{'Improving' if mode == 'improve' else 'Generating'} task prompt using {ai_provider}",
        )

        prompt = build_task_prompt(user_prompt, mode)
        result = self._generate(prompt, ai_provider, ai_settings or {})

        if result["success"]:
            self._log(
                "info", f"Successfully generated task: {result['data'].get('name', 'Unknown')}"
            )
        else:
            self._log("error", f"Task prompt generation failed: {result['error']}")

        return result

    def suggest_exploration_strategy(
        self,
        user_goal: str,
        available_states: list[dict],
        available_transitions: list[dict],
        ai_provider: str = "claude_cli",
        ai_settings: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Suggest an exploration strategy based on user goal."""
        self._log("info", f"Suggesting exploration strategy using {ai_provider}")

        prompt = build_exploration_prompt(user_goal, available_states, available_transitions)
        result = self._generate(prompt, ai_provider, ai_settings or {})

        if result["success"]:
            self._log(
                "info",
                f"Successfully suggested strategy: {result['data'].get('strategy', 'Unknown')}",
            )
        else:
            self._log("error", f"Exploration suggestion failed: {result['error']}")

        return result

    def generate_test_and_agentic_step(
        self,
        user_prompt: str,
        page_context: dict | None = None,
        contexts_content: str | None = None,
        ai_provider: str = "claude_cli",
        ai_settings: dict[str, Any] | None = None,
        max_retries: int = 2,
    ) -> dict[str, Any]:
        """Generate a verification test and agentic step from user instructions.

        Validates that generated tests use only valid selectors from the page context.
        Will retry up to max_retries times if invalid selectors are detected.
        """
        self._log("info", f"Generating test and agentic step using {ai_provider}")
        if contexts_content:
            self._log(
                "info", f"Including {contexts_content.count('<context')} context(s) in prompt"
            )

        # Log page context structure for debugging
        if page_context:
            has_pages = "pages" in page_context and page_context.get("pages")
            has_elements = "elements" in page_context and page_context.get("elements")
            self._log(
                "info",
                f"[SELECTOR VALIDATION] page_context structure: has_pages={has_pages}, has_elements={has_elements}, keys={list(page_context.keys())}",
            )

        # Extract valid IDs from page context for validation
        valid_ids: set[str] = set()
        if page_context:
            pages = page_context.get("pages")
            if pages and isinstance(pages, list):
                self._log(
                    "info", f"[SELECTOR VALIDATION] Multi-page context with {len(pages)} pages"
                )
                for page in pages:
                    page_elements = page.get("elements") or []
                    page_ids = _extract_valid_ids(page_elements)
                    self._log(
                        "info",
                        f"[SELECTOR VALIDATION] Page '{page.get('title', 'Unknown')}': {len(page_ids)} IDs",
                    )
                    valid_ids.update(page_ids)
            else:
                elements = page_context.get("elements") or []
                valid_ids.update(_extract_valid_ids(elements))
                self._log(
                    "info", f"[SELECTOR VALIDATION] Single page context: {len(valid_ids)} IDs"
                )
        else:
            self._log(
                "info", "[SELECTOR VALIDATION] No page context provided - skipping validation"
            )

        self._log("info", f"[SELECTOR VALIDATION] Total valid IDs: {len(valid_ids)}")
        if valid_ids:
            # Log a sample of valid IDs for debugging
            sample_ids = sorted(valid_ids)[:10]
            self._log("info", f"[SELECTOR VALIDATION] Sample valid IDs: {sample_ids}")

        prompt = build_test_and_agentic_prompt(user_prompt, page_context, contexts_content)

        for attempt in range(max_retries + 1):
            result = self._generate(prompt, ai_provider, ai_settings or {})

            if not result["success"]:
                self._log("error", f"Test generation failed: {result['error']}")
                return result

            test_code = result["data"].get("verification_test", "")
            test_name = result["data"].get("test_name", "Unknown")
            agentic_name = result["data"].get("agentic_name", "Unknown")

            # Validate selectors if we have valid IDs to check against
            if valid_ids:
                self._log(
                    "info", f"[SELECTOR VALIDATION] Validating test code (attempt {attempt + 1})"
                )
                is_valid, invalid_ids = _validate_test_selectors(test_code, valid_ids)
                self._log(
                    "info",
                    f"[SELECTOR VALIDATION] Validation result: is_valid={is_valid}, invalid_ids={invalid_ids}",
                )

                if not is_valid:
                    self._log(
                        "warning",
                        f"[SELECTOR VALIDATION] Attempt {attempt + 1}/{max_retries + 1}: Invalid selectors detected: {invalid_ids}",
                    )

                    if attempt < max_retries:
                        self._log(
                            "info",
                            f"[SELECTOR VALIDATION] Triggering retry {attempt + 2}/{max_retries + 1}",
                        )
                        # Build retry prompt with explicit correction
                        valid_ids_list = "\n".join(
                            [f'  - "{el_id}"' for el_id in sorted(valid_ids)]
                        )
                        correction_prompt = f"""## RETRY - Your previous attempt used INVALID selectors

The following selectors you used do NOT exist and will cause test failure:
{chr(10).join([f'  - "{inv_id}" (INVALID)' for inv_id in invalid_ids])}

## VALID IDs you MUST use instead (copy-paste from this list):
{valid_ids_list}

Please regenerate the test using ONLY the valid IDs listed above.
Replace each invalid selector with the closest matching valid ID.

## Original request:
{user_prompt}

{build_test_and_agentic_prompt(user_prompt, page_context, contexts_content)}"""
                        prompt = correction_prompt
                        continue
                    else:
                        # Max retries reached, include warning in result
                        result["data"]["_selector_warnings"] = invalid_ids
                        self._log(
                            "warning",
                            f"Max retries reached. Test has invalid selectors: {invalid_ids}",
                        )

            self._log(
                "info",
                f"Successfully generated test '{test_name}' and agentic step '{agentic_name}'",
            )
            return result

        return result

    def explore_flow_step(
        self,
        user_prompt: str,
        current_elements: list[dict],
        current_url: str,
        current_title: str,
        captured_pages: list[dict],
        step_number: int,
        ai_provider: str = "claude_cli",
        ai_settings: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Determine the next action in an AI-driven flow exploration.

        The AI analyzes current page elements and user's goal to decide what
        action to take next: click, type, wait, or done.
        """
        self._log("info", f"Flow exploration step {step_number} using {ai_provider}")
        self._log("debug", f"User prompt: {user_prompt}")
        self._log("debug", f"Current page: {current_title} ({current_url})")
        self._log("debug", f"Total elements: {len(current_elements)}")

        # Log element IDs and text for debugging
        for el in current_elements[:20]:  # First 20 elements
            el_id = el.get("id", "")
            el_text = (el.get("text") or el.get("label") or "")[:50]
            el_tag = el.get("tagName", "")
            self._log("debug", f"  Element: id={el_id!r} tag={el_tag} text={el_text!r}")

        prompt = build_flow_exploration_prompt(
            user_prompt=user_prompt,
            current_elements=current_elements,
            current_url=current_url,
            current_title=current_title,
            captured_pages=captured_pages,
            step_number=step_number,
        )
        result = self._generate(prompt, ai_provider, ai_settings or {})

        if result["success"]:
            action = result["data"].get("action", "unknown")
            element_id = result["data"].get("element_id", "none")
            element_desc = result["data"].get("element_description", "")
            reason = result["data"].get("reason", "")
            self._log("info", f"AI decided: {action} element_id={element_id!r}")
            self._log("info", f"AI reason: {reason}")
            if element_desc:
                self._log("info", f"AI element description: {element_desc}")
        else:
            self._log("error", f"Flow exploration step failed: {result['error']}")

        return result


def build_flow_exploration_prompt(
    user_prompt: str,
    current_elements: list[dict],
    current_url: str,
    current_title: str,
    captured_pages: list[dict],
    step_number: int,
) -> str:
    """Build prompt for AI to decide the next flow exploration action."""

    # Format current page elements - focus on interactive ones
    interactive_elements = []
    all_elements = []

    for el in current_elements:
        tag = el.get("tagName", "").lower()
        el_type = el.get("type", "")
        el_id = el.get("id", "")
        el_text = (el.get("text") or el.get("label") or "")[:80]
        visible = el.get("visible", True)
        enabled = el.get("enabled", True)

        if not visible:
            continue

        # Build element description
        desc_parts = [f"`{tag}`"]
        if el_id:
            desc_parts.append(f'id="{el_id}"')
        if el_type:
            desc_parts.append(f'type="{el_type}"')
        if el_text:
            desc_parts.append(f'text="{el_text}"')
        if not enabled:
            desc_parts.append("(disabled)")

        el_desc = " ".join(desc_parts)
        all_elements.append(f"  - {el_desc}")

        # Track interactive elements separately
        if tag in ("button", "a", "input", "select", "textarea") or el.get("role") in (
            "button",
            "link",
            "menuitem",
            "tab",
        ):
            interactive_elements.append(f"  - {el_desc}")

    # Show interactive elements prominently, then others
    elements_section = ""
    if interactive_elements:
        elements_section += "### Interactive Elements (buttons, links, inputs)\n"
        elements_section += "\n".join(interactive_elements[:30])  # Limit for token efficiency
        elements_section += "\n"

    if all_elements:
        elements_section += f"\n### All Visible Elements ({len(all_elements)} total)\n"
        elements_section += "\n".join(all_elements[:50])  # Limit

    # Format captured pages summary with URL to help detect same-URL tab navigation
    captured_summary = ""
    if captured_pages:
        captured_lines = [
            f"  - Page {i + 1}: {p.get('title', 'Unknown')} - {p.get('url', 'Unknown')} ({p.get('element_count', 0)} elements)"
            for i, p in enumerate(captured_pages)
        ]
        captured_summary = f"""
### Already Captured Pages
{chr(10).join(captured_lines)}
"""

    # Check if current state matches any already captured (same URL + similar element count)
    current_element_count = len(current_elements)
    already_captured = False
    for p in captured_pages:
        if p.get("url") == current_url:
            # Same URL - check if element count is similar (within 10% = same tab state)
            prev_count = p.get("element_count", 0)
            if prev_count > 0 and abs(current_element_count - prev_count) / prev_count < 0.1:
                already_captured = True
                break

    already_captured_note = ""
    if already_captured:
        already_captured_note = """
**IMPORTANT**: The current page state appears to have already been captured (same URL and similar element count).
If you've already captured what the user asked for, return `done`. Do NOT click the same element again.
"""

    return f"""## Task
You are helping navigate a web application to capture elements from multiple pages.
The user wants to explore a flow and capture page elements along the way.

## User's Goal
{user_prompt}

## Current State
- **Step Number**: {step_number}
- **Current Page**: {current_title}
- **Current URL**: {current_url}
- **Current Element Count**: {current_element_count}
{captured_summary}{already_captured_note}
## Current Page Elements
{elements_section}

## Your Task
Decide what action to take next to help achieve the user's goal.

**Available Actions:**
1. `click` - Click an element (provide element_id)
2. `type` - Type text into an input (provide element_id and text)
3. `wait` - Wait for content to load (provide wait_ms)
4. `done` - Goal achieved, exploration complete

**Guidelines:**
- If the current page is the target destination mentioned in the goal, return `done`
- If you need to click a button or link to navigate, use `click` with the exact element ID
- Use `should_capture_after: true` if clicking will navigate to a new page you want to capture
- Match elements by their text content, ID, or role to find what the user described
- If you can't find the element the user described, explain why in `reason`
- **Tab Navigation**: When clicking tabs on a single-page app, the URL may stay the same but the content changes. If the element count changes significantly after clicking, the tab has switched successfully.
- **Avoid Duplicates**: Check the "Already Captured Pages" list. If a page with the same URL and similar element count is already captured, don't capture it again.

## Output Format
Return a JSON object with exactly these fields:
```json
{{
  "action": "click",
  "element_id": "actual-element-id-from-above",
  "element_description": "Description of what element you're clicking",
  "text": null,
  "wait_ms": null,
  "reason": "Clicking this button to navigate to the results page",
  "should_capture_after": true
}}
```

For `done`:
```json
{{
  "action": "done",
  "element_id": null,
  "element_description": null,
  "text": null,
  "wait_ms": null,
  "reason": "Successfully reached the results page, goal achieved",
  "should_capture_after": false
}}
```

Return ONLY the JSON object, no explanations or markdown code blocks."""
