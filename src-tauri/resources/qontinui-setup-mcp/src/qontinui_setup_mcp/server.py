"""MCP server for guided qontinui-runner setup and configuration."""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any

from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import (
    GetPromptResult,
    Prompt,
    PromptArgument,
    PromptMessage,
    TextContent,
    Tool,
)

from qontinui_setup_mcp.client import RunnerClient
from qontinui_setup_mcp.configuration.ai_provider import (
    check_api_key,
    get_ai_settings,
    set_ai_provider,
    store_api_key,
    test_ai_connection,
)

# ── Imports: configuration ──────────────────────────────────────────────
from qontinui_setup_mcp.configuration.log_sources import (
    add_log_source,
    apply_suggested_sources,
    get_log_sources,
    remove_log_source,
    update_log_source,
)
from qontinui_setup_mcp.configuration.profiles import (
    create_log_profile,
    set_default_profile,
)
from qontinui_setup_mcp.discovery.frameworks import detect_framework
from qontinui_setup_mcp.discovery.log_finder import find_log_files, suggest_log_sources

# ── Imports: discovery ──────────────────────────────────────────────────
from qontinui_setup_mcp.discovery.scanner import scan_workspace

# ── Imports: guidance ───────────────────────────────────────────────────
from qontinui_setup_mcp.guidance.logging_advice import get_logging_advice
from qontinui_setup_mcp.validation.connection import (
    check_runner_connection,
    get_setup_status,
)
from qontinui_setup_mcp.validation.log_validator import validate_log_sources

# ── Imports: validation ─────────────────────────────────────────────────
from qontinui_setup_mcp.validation.prerequisites import check_prerequisites

logger = logging.getLogger(__name__)

# ── Global state ────────────────────────────────────────────────────────

server = Server("qontinui-setup-mcp")
client: RunnerClient | None = None


def _get_client() -> RunnerClient:
    global client
    if client is None:
        client = RunnerClient()
    return client


def _text(data: Any) -> list[TextContent]:
    """Wrap a result as JSON TextContent."""
    return [TextContent(type="text", text=json.dumps(data, indent=2, default=str))]


# ═══════════════════════════════════════════════════════════════════════
#  TOOL DEFINITIONS
# ═══════════════════════════════════════════════════════════════════════

TOOLS: list[Tool] = [
    # ── Discovery (LOCAL) ───────────────────────────────────────────────
    Tool(
        name="scan_workspace",
        description=(
            "Scan a directory tree for software projects. "
            "Detects package.json, pyproject.toml, Cargo.toml, go.mod, etc. "
            "Works offline — does not require the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to scan (absolute path)",
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Max directory depth to traverse (default 3)",
                    "default": 3,
                },
            },
            "required": ["path"],
        },
    ),
    Tool(
        name="detect_framework",
        description=(
            "Analyze a project to identify its framework, language, and build tool. "
            "Parses manifest files (package.json, pyproject.toml, etc.) and checks dependencies. "
            "Works offline."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Path to the project root",
                },
            },
            "required": ["project_path"],
        },
    ),
    Tool(
        name="find_log_files",
        description=(
            "Find existing log files and log directories in a project. "
            "Looks for *.log, *.jsonl, common log directories, etc. "
            "Works offline."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Path to the project root",
                },
            },
            "required": ["project_path"],
        },
    ),
    Tool(
        name="suggest_log_sources",
        description=(
            "Generate ready-to-use log source configurations for a project. "
            "Combines framework detection with log file discovery. "
            "Returns configs compatible with the runner's log source settings. "
            "Works offline."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Path to the project root",
                },
            },
            "required": ["project_path"],
        },
    ),
    # ── Log Source Configuration (REQUIRE runner) ───────────────────────
    Tool(
        name="get_log_sources",
        description=(
            "Get all configured log sources from the runner. "
            "Returns the full log source settings including sources, profiles, and AI selection mode. "
            "Requires the runner to be running."
        ),
        inputSchema={
            "type": "object",
            "properties": {},
        },
    ),
    Tool(
        name="add_log_source",
        description=(
            "Add a new log source to the runner configuration. "
            "Provide the source details; an ID will be generated if not included. "
            "Requires the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Display name for the log source",
                },
                "path": {
                    "type": "string",
                    "description": "Absolute path to the log file or directory",
                },
                "category": {
                    "type": "string",
                    "enum": [
                        "frontend",
                        "backend",
                        "api",
                        "mobile",
                        "database",
                        "build",
                        "testing",
                        "runner",
                        "general",
                    ],
                    "description": "Log source category",
                },
                "format": {
                    "type": "string",
                    "enum": ["plaintext", "json", "jsonl"],
                    "description": "Log format (default: plaintext)",
                    "default": "plaintext",
                },
                "parser": {
                    "type": "string",
                    "enum": ["python", "javascript", "rust", "generic"],
                    "description": "Log parser (default: generic)",
                    "default": "generic",
                },
                "type": {
                    "type": "string",
                    "enum": ["file", "directory"],
                    "description": "Whether the path is a file or directory (default: file)",
                    "default": "file",
                },
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern for matching files in a directory (e.g. '*.log')",
                },
                "keywords": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Keywords for log relevance filtering",
                },
                "error_patterns": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Regex patterns to identify error lines",
                },
                "description": {
                    "type": "string",
                    "description": "Optional description of the log source",
                },
            },
            "required": ["name", "path", "category"],
        },
    ),
    Tool(
        name="update_log_source",
        description=(
            "Update an existing log source by ID. "
            "Only include the fields you want to change. "
            "Requires the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "source_id": {
                    "type": "string",
                    "description": "ID of the log source to update",
                },
                "name": {"type": "string"},
                "path": {"type": "string"},
                "category": {"type": "string"},
                "format": {"type": "string"},
                "parser": {"type": "string"},
                "enabled": {"type": "boolean"},
                "keywords": {"type": "array", "items": {"type": "string"}},
                "error_patterns": {"type": "array", "items": {"type": "string"}},
                "warning_patterns": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["source_id"],
        },
    ),
    Tool(
        name="remove_log_source",
        description="Remove a log source by ID. Also removes it from any profiles. Requires the runner.",
        inputSchema={
            "type": "object",
            "properties": {
                "source_id": {
                    "type": "string",
                    "description": "ID of the log source to remove",
                },
            },
            "required": ["source_id"],
        },
    ),
    Tool(
        name="apply_suggested_sources",
        description=(
            "Discover log sources for a project and add them all to the runner in one step. "
            "Combines suggest_log_sources + add. Use dry_run=true to preview without changes. "
            "Requires the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Path to the project root",
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, return what would be added without making changes (default: false)",
                    "default": False,
                },
            },
            "required": ["project_path"],
        },
    ),
    # ── Log Source Profiles (REQUIRE runner) ────────────────────────────
    Tool(
        name="create_log_profile",
        description=(
            "Create a named profile that groups log sources together. "
            "Profiles let you switch between different sets of log sources. "
            "Requires the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Profile display name"},
                "source_ids": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "IDs of log sources to include in this profile",
                },
                "description": {
                    "type": "string",
                    "description": "Optional profile description",
                },
            },
            "required": ["name", "source_ids"],
        },
    ),
    Tool(
        name="set_default_profile",
        description="Set the default active log source profile. Pass null to clear. Requires the runner.",
        inputSchema={
            "type": "object",
            "properties": {
                "profile_id": {
                    "type": ["string", "null"],
                    "description": "Profile ID to set as default, or null to clear",
                },
            },
            "required": ["profile_id"],
        },
    ),
    # ── AI Provider (REQUIRE runner) ────────────────────────────────────
    Tool(
        name="get_ai_settings",
        description=(
            "Get the current AI provider configuration from the runner. "
            "Returns provider, model, and execution mode settings (no API keys). "
            "Requires the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {},
        },
    ),
    Tool(
        name="set_ai_provider",
        description=(
            "Configure the AI provider and settings. "
            "Supports claude_cli, claude_api, gemini_cli, gemini_api. "
            "Requires the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "enum": ["claude_cli", "claude_api", "gemini_cli", "gemini_api"],
                    "description": "AI provider to use",
                },
                "model": {
                    "type": "string",
                    "description": "Model name (e.g. 'claude-sonnet-4-20250514', 'gemini-3-flash-preview')",
                },
                "cli_execution_mode": {
                    "type": "string",
                    "enum": ["auto", "windows_native", "wsl", "native"],
                    "description": "CLI execution mode (only for *_cli providers)",
                },
            },
            "required": ["provider"],
        },
    ),
    Tool(
        name="store_api_key",
        description=(
            "Store an API key in the OS keychain for a provider. "
            "The key is stored securely and never returned by the API. "
            "Requires the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider name (e.g. 'claude_api', 'gemini_api')",
                },
                "api_key": {"type": "string", "description": "The API key to store"},
            },
            "required": ["provider", "api_key"],
        },
    ),
    Tool(
        name="check_api_key",
        description="Check if an API key exists in the keychain for a provider. Requires the runner.",
        inputSchema={
            "type": "object",
            "properties": {
                "provider": {"type": "string", "description": "Provider name to check"},
            },
            "required": ["provider"],
        },
    ),
    Tool(
        name="test_ai_connection",
        description="Test AI connectivity with the currently configured provider. Requires the runner.",
        inputSchema={
            "type": "object",
            "properties": {},
        },
    ),
    # ── Validation (LOCAL + runner hybrid) ──────────────────────────────
    Tool(
        name="check_prerequisites",
        description=(
            "Check if required system tools are installed (node, python, rust, git, etc.). "
            "Reports version and path for each tool. "
            "Works offline."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "checks": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": (
                        "Specific tools to check. If omitted, checks all. "
                        "Options: node, python, rust, cargo, git, npm, yarn, poetry, claude_cli, gemini_cli, docker"
                    ),
                },
            },
        },
    ),
    Tool(
        name="check_runner_connection",
        description="Test connectivity to the qontinui-runner HTTP API.",
        inputSchema={
            "type": "object",
            "properties": {
                "host": {
                    "type": "string",
                    "description": "Runner host (default: auto-detect)",
                },
                "port": {
                    "type": "integer",
                    "description": "Runner port (default: 9876)",
                },
            },
        },
    ),
    Tool(
        name="validate_log_sources",
        description=(
            "Validate all configured log sources — check paths exist, are readable, and have recent data. "
            "Requires the runner."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "check_freshness": {
                    "type": "boolean",
                    "description": "Check if log files were modified in the last 24 hours (default: true)",
                    "default": True,
                },
            },
        },
    ),
    Tool(
        name="get_setup_status",
        description=(
            "Get a full setup overview with completion percentage and recommendations. "
            "Checks runner connectivity, AI config, log sources, prerequisites, and more. "
            "Returns partial results if the runner is not connected."
        ),
        inputSchema={
            "type": "object",
            "properties": {},
        },
    ),
    # ── Guidance (LOCAL) ────────────────────────────────────────────────
    Tool(
        name="get_logging_advice",
        description=(
            "Get framework-specific instructions for setting up file-based logging. "
            "Many frameworks (Next.js, React, etc.) only log to stdout by default. "
            "This tool explains how to configure them for file logging. "
            "Works offline."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "framework": {
                    "type": "string",
                    "description": (
                        "Framework key (e.g. 'nextjs', 'fastapi', 'django', 'express', 'react_vite', "
                        "'nestjs', 'flask', 'rust_cargo', 'go', 'spring_boot', 'rails', 'tauri', "
                        "'react_native', 'flutter')"
                    ),
                },
            },
            "required": ["framework"],
        },
    ),
]


# ═══════════════════════════════════════════════════════════════════════
#  MCP PROMPTS
# ═══════════════════════════════════════════════════════════════════════

PROMPTS: list[Prompt] = [
    Prompt(
        name="setup_runner_for_project",
        description=(
            "Full guided setup workflow for configuring the qontinui-runner for a project. "
            "Discovers the project, suggests log sources, checks prerequisites, and validates the setup."
        ),
        arguments=[
            PromptArgument(
                name="project_path",
                description="Absolute path to the project to set up",
                required=True,
            ),
        ],
    ),
    Prompt(
        name="diagnose_setup_issues",
        description=(
            "Analyze the current runner setup, find problems, and suggest fixes. "
            "Checks connectivity, validates log sources, verifies AI config, and checks prerequisites."
        ),
        arguments=[],
    ),
    Prompt(
        name="add_project_logs",
        description=(
            "Quick workflow to discover and add log sources for a project. "
            "Scans the project, suggests sources, and applies them."
        ),
        arguments=[
            PromptArgument(
                name="project_path",
                description="Absolute path to the project",
                required=True,
            ),
        ],
    ),
]


# ═══════════════════════════════════════════════════════════════════════
#  HANDLERS
# ═══════════════════════════════════════════════════════════════════════


@server.list_tools()  # type: ignore[untyped-decorator, no-untyped-call]
async def list_tools() -> list[Tool]:
    return TOOLS


@server.call_tool()  # type: ignore[untyped-decorator]
async def call_tool(name: str, arguments: dict[str, Any]) -> list[TextContent]:
    """Dispatch tool calls to their implementations."""
    try:
        result = await _dispatch_tool(name, arguments)
        return _text(result)
    except Exception as e:
        logger.exception("Tool %s failed", name)
        return _text({"error": str(e)})


async def _dispatch_tool(name: str, args: dict[str, Any]) -> Any:
    """Route a tool call to the correct function."""
    c = _get_client()

    # ── Discovery (LOCAL) ───────────────────────────────────────────
    if name == "scan_workspace":
        return await scan_workspace(
            path=args["path"],
            max_depth=args.get("max_depth", 3),
        )

    if name == "detect_framework":
        return await detect_framework(project_path=args["project_path"])

    if name == "find_log_files":
        return await find_log_files(project_path=args["project_path"])

    if name == "suggest_log_sources":
        return await suggest_log_sources(project_path=args["project_path"])

    # ── Log Source Configuration (REQUIRE runner) ───────────────────
    if name == "get_log_sources":
        return await get_log_sources(c)

    if name == "add_log_source":
        source: dict[str, Any] = {k: v for k, v in args.items() if v is not None}
        return await add_log_source(c, source)

    if name == "update_log_source":
        source_id = args["source_id"]
        updates = {k: v for k, v in args.items() if k != "source_id" and v is not None}
        return await update_log_source(c, source_id, updates)

    if name == "remove_log_source":
        return await remove_log_source(c, args["source_id"])

    if name == "apply_suggested_sources":
        return await apply_suggested_sources(
            c,
            project_path=args["project_path"],
            dry_run=args.get("dry_run", False),
        )

    # ── Log Source Profiles (REQUIRE runner) ────────────────────────
    if name == "create_log_profile":
        return await create_log_profile(
            c,
            name=args["name"],
            source_ids=args["source_ids"],
            description=args.get("description"),
        )

    if name == "set_default_profile":
        return await set_default_profile(c, profile_id=args["profile_id"])

    # ── AI Provider (REQUIRE runner) ────────────────────────────────
    if name == "get_ai_settings":
        return await get_ai_settings(c)

    if name == "set_ai_provider":
        return await set_ai_provider(
            c,
            provider=args["provider"],
            model=args.get("model"),
            cli_execution_mode=args.get("cli_execution_mode"),
        )

    if name == "store_api_key":
        return await store_api_key(
            c, provider=args["provider"], api_key=args["api_key"]
        )

    if name == "check_api_key":
        return await check_api_key(c, provider=args["provider"])

    if name == "test_ai_connection":
        return await test_ai_connection(c)

    # ── Validation (LOCAL + runner hybrid) ──────────────────────────
    if name == "check_prerequisites":
        return await check_prerequisites(checks=args.get("checks"))

    if name == "check_runner_connection":
        rc = (
            RunnerClient(
                host=args["host"],
                port=args["port"],
            )
            if ("host" in args or "port" in args)
            else c
        )
        return await check_runner_connection(rc)

    if name == "validate_log_sources":
        return await validate_log_sources(
            c, check_freshness=args.get("check_freshness", True)
        )

    if name == "get_setup_status":
        return await get_setup_status(c)

    # ── Guidance (LOCAL) ────────────────────────────────────────────
    if name == "get_logging_advice":
        return await get_logging_advice(framework=args["framework"])

    return {"error": f"Unknown tool: {name}"}


# ═══════════════════════════════════════════════════════════════════════
#  PROMPT HANDLERS
# ═══════════════════════════════════════════════════════════════════════


@server.list_prompts()  # type: ignore[untyped-decorator, no-untyped-call]
async def list_prompts() -> list[Prompt]:
    return PROMPTS


@server.get_prompt()  # type: ignore[untyped-decorator, no-untyped-call]
async def get_prompt(name: str, arguments: dict[str, str] | None) -> GetPromptResult:
    """Build prompt content for each prompt template."""
    args = arguments or {}

    if name == "setup_runner_for_project":
        return await _build_setup_prompt(args.get("project_path", "."))

    if name == "diagnose_setup_issues":
        return await _build_diagnose_prompt()

    if name == "add_project_logs":
        return await _build_add_logs_prompt(args.get("project_path", "."))

    return GetPromptResult(
        description=f"Unknown prompt: {name}",
        messages=[
            PromptMessage(
                role="user",
                content=TextContent(type="text", text=f"Unknown prompt: {name}"),
            )
        ],
    )


async def _build_setup_prompt(project_path: str) -> GetPromptResult:
    """Build the full guided setup prompt."""
    c = _get_client()

    # Gather context in parallel
    framework_task = detect_framework(project_path)
    suggestions_task = suggest_log_sources(project_path)
    prereqs_task = check_prerequisites()
    connection_task = check_runner_connection(c)

    framework, suggestions, prereqs, connection = await asyncio.gather(
        framework_task, suggestions_task, prereqs_task, connection_task
    )

    context_parts = [
        f"# Runner Setup for Project: {project_path}\n",
        f"## Framework Detection\n```json\n{json.dumps(framework, indent=2, default=str)}\n```\n",
        f"## Suggested Log Sources\n```json\n{json.dumps(suggestions, indent=2, default=str)}\n```\n",
        f"## Prerequisites\n```json\n{json.dumps(prereqs, indent=2, default=str)}\n```\n",
        f"## Runner Connection\n```json\n{json.dumps(connection, indent=2, default=str)}\n```\n",
    ]

    if suggestions.get("needs_logging_setup"):
        key = framework.get("key", "")
        if key:
            advice = await get_logging_advice(key)
            context_parts.append(
                f"## Logging Setup Advice\n```json\n{json.dumps(advice, indent=2, default=str)}\n```\n"
            )

    context = "\n".join(context_parts)

    instructions = (
        "You are helping set up the qontinui-runner for this project. "
        "Based on the context above:\n\n"
        "1. Review the detected framework and prerequisites\n"
        "2. If the runner is connected, apply the suggested log sources (use apply_suggested_sources)\n"
        "3. If logging setup is needed, guide the user through configuring file-based logging\n"
        "4. Check AI provider configuration and help set it up if needed\n"
        "5. Run get_setup_status to verify everything is configured\n"
        "6. Provide a summary of what was done and any remaining steps"
    )

    return GetPromptResult(
        description=f"Setup runner for {project_path}",
        messages=[
            PromptMessage(
                role="user",
                content=TextContent(type="text", text=f"{context}\n\n{instructions}"),
            )
        ],
    )


async def _build_diagnose_prompt() -> GetPromptResult:
    """Build the diagnosis prompt with current state."""
    c = _get_client()

    status = await get_setup_status(c)

    context = f"# Current Setup Status\n```json\n{json.dumps(status, indent=2, default=str)}\n```\n"

    # If connected, also validate log sources
    if status.get("runner_connected"):
        validation = await validate_log_sources(c)
        context += f"\n## Log Source Validation\n```json\n{json.dumps(validation, indent=2, default=str)}\n```\n"

    instructions = (
        "You are diagnosing the qontinui-runner setup. "
        "Based on the context above:\n\n"
        "1. Identify any issues (failed checks, invalid log sources, missing config)\n"
        "2. For each issue, explain the problem and provide a fix\n"
        "3. Use the available tools to fix issues where possible\n"
        "4. Run get_setup_status again after fixes to verify improvement"
    )

    return GetPromptResult(
        description="Diagnose runner setup issues",
        messages=[
            PromptMessage(
                role="user",
                content=TextContent(type="text", text=f"{context}\n\n{instructions}"),
            )
        ],
    )


async def _build_add_logs_prompt(project_path: str) -> GetPromptResult:
    """Build the quick add-logs prompt."""
    suggestions = await suggest_log_sources(project_path)

    context = f"# Log Source Discovery for {project_path}\n```json\n{json.dumps(suggestions, indent=2, default=str)}\n```\n"

    instructions = (
        "You are adding log sources for this project to the qontinui-runner. "
        "Based on the discovery results:\n\n"
        "1. Review the suggested log sources\n"
        "2. Apply them using apply_suggested_sources (consider a dry_run first)\n"
        "3. If logging setup is needed, provide guidance using get_logging_advice\n"
        "4. Validate the new sources with validate_log_sources"
    )

    return GetPromptResult(
        description=f"Add log sources for {project_path}",
        messages=[
            PromptMessage(
                role="user",
                content=TextContent(type="text", text=f"{context}\n\n{instructions}"),
            )
        ],
    )


# ═══════════════════════════════════════════════════════════════════════
#  ENTRY POINT
# ═══════════════════════════════════════════════════════════════════════


async def main() -> None:
    """Run the MCP server on stdio."""
    logger.info("Starting qontinui-setup-mcp server")
    async with stdio_server() as (read_stream, write_stream):
        await server.run(
            read_stream, write_stream, server.create_initialization_options()
        )


def run() -> None:
    """Entry point for the MCP server."""
    asyncio.run(main())
