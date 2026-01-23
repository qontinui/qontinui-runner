"""AI Builder Generator Service.

Generates content for builder tabs using AI:
- Context (knowledge base entries)
- API Request templates
- Task prompts
- Exploration strategies
"""

import json
import logging
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

    if cli_result["success"]:
        parsed = _parse_json_output(cli_result["output"])
        if parsed["success"]:
            result["success"] = True
            result["data"] = parsed["data"]
        else:
            result["error"] = parsed["error"]
    else:
        result["error"] = cli_result["error"]

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
    output = output.strip()

    # Remove markdown code blocks if present
    if output.startswith("```"):
        lines = output.split("\n")
        lines = lines[1:]
        if lines and lines[-1].strip() == "```":
            lines = lines[:-1]
        output = "\n".join(lines).strip()

    try:
        data = json.loads(output)
        return {"success": True, "data": data, "error": ""}
    except json.JSONDecodeError as e:
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
