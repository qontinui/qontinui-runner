"""AI generation commands (``*_with_ai``, agentic step, explore-flow step).

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

from typing import Any, ClassVar

from ._host import ExecutorHost
from ._shared import (
    AiBuilderGeneratorService,
    AiShellCommandGeneratorService,
    AiTestGeneratorService,
)


class AiGenerateCommands(ExecutorHost):
    """AI generation commands (``*_with_ai``, agentic step, explore-flow step)."""

    COMMANDS: ClassVar[dict[str, str]] = {
        "generate_test_with_ai": "_handle_generate_test_with_ai",
        # AI shell command generation
        "generate_shell_command_with_ai": "_handle_generate_shell_command_with_ai",
        # AI builder generation commands
        "generate_context_with_ai": "_handle_generate_context_with_ai",
        "generate_api_request_with_ai": "_handle_generate_api_request_with_ai",
        "generate_task_prompt_with_ai": "_handle_generate_task_prompt_with_ai",
        "suggest_exploration_strategy_with_ai": "_handle_suggest_exploration_strategy_with_ai",
        "generate_test_and_agentic_step": "_handle_generate_test_and_agentic_step",
        "explore_flow_step": "_handle_explore_flow_step",
    }

    def _get_ai_test_generator_service(self) -> AiTestGeneratorService:
        """Get or create the AI test generator service."""
        if self._ai_test_generator_service is None:
            self._ai_test_generator_service = AiTestGeneratorService(
                event_manager=self.event_manager,
            )
        return self._ai_test_generator_service  # type: ignore[no-any-return]

    def _handle_generate_test_with_ai(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Handle AI test generation command.

        Uses the configured AI provider to generate test code from a natural
        language description and optional page analysis context.

        Args:
            params: Dict with:
                - user_prompt: str - User's description of the test
                - test_type: str - Type of test (playwright_cdp, qontinui_vision, etc.)
                - page_analysis: dict | None - Optional page analysis data
                - multi_request_analysis: dict | None - Optional multi-request ground truth
                - collected_analyses: dict | None - Optional collected analyses (multiple types)
                - ai_provider: str - AI provider (claude_cli, claude_api, gemini_cli, gemini_api)
                - ai_settings: dict | None - Provider-specific settings

        Returns:
            Dict with success, code, and error fields.
        """
        import sys

        user_prompt = params.get("user_prompt", "")
        test_type = params.get("test_type", "playwright_cdp")
        page_analysis = params.get("page_analysis")
        multi_request_analysis = params.get("multi_request_analysis")
        collected_analyses = params.get("collected_analyses")
        reference_documents = params.get("reference_documents")
        workflow_run_context = params.get("workflow_run_context")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        print(
            f"[info    ] EXECUTOR: _handle_generate_test_with_ai called with provider={ai_provider}, test_type={test_type}",
            file=sys.stderr,
            flush=True,
        )

        if not user_prompt:
            return {
                "success": False,
                "error": "user_prompt is required",
            }

        try:
            service = self._get_ai_test_generator_service()

            result = service.generate_test(
                user_prompt=user_prompt,
                test_type=test_type,
                page_analysis=page_analysis,
                multi_request_analysis=multi_request_analysis,
                collected_analyses=collected_analyses,
                reference_documents=reference_documents,
                workflow_run_context=workflow_run_context,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )

            return result

        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"AI test generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _get_ai_shell_command_generator_service(self) -> AiShellCommandGeneratorService:
        """Get or create the AI shell command generator service."""
        if self._ai_shell_command_generator_service is None:
            self._ai_shell_command_generator_service = AiShellCommandGeneratorService(
                event_manager=self.event_manager,
            )
        return self._ai_shell_command_generator_service  # type: ignore[no-any-return]

    def _handle_generate_shell_command_with_ai(self, params: dict[str, Any]) -> dict[str, Any]:
        """
        Handle AI shell command generation.

        Uses the configured AI provider to generate shell commands from a natural
        language description.

        Args:
            params: Dict with:
                - user_prompt: str - User's description of the command
                - target_os: str - Target OS (windows, linux, macos)
                - category: str | None - Command category (git, npm, docker, etc.)
                - ai_provider: str - AI provider (claude_cli, claude_api, gemini_cli, gemini_api)
                - ai_settings: dict | None - Provider-specific settings

        Returns:
            Dict with success, command, description, and error fields.
        """
        import datetime
        import os
        import sys

        # Debug log file. ~ expands from USERPROFILE on Windows and HOME
        # elsewhere; the old literal fallback named one machine's Windows
        # account, so on any other login this aimed at a profile that does not
        # exist (and os.makedirs below would then create it).
        debug_log = os.path.join(
            os.path.expanduser("~"),
            ".qontinui",
            "ai-shell-debug.log",
        )
        os.makedirs(os.path.dirname(debug_log), exist_ok=True)

        def debug(msg):
            ts = datetime.datetime.now().isoformat()
            line = f"[{ts}] {msg}\n"
            with open(debug_log, "a", encoding="utf-8") as f:
                f.write(line)
            print(f"[DEBUG] {msg}", file=sys.stderr, flush=True)

        debug("=" * 60)
        debug("_handle_generate_shell_command_with_ai CALLED")
        debug(f"params keys: {list(params.keys())}")

        user_prompt = params.get("user_prompt", "")
        target_os = params.get("target_os", "windows")
        category = params.get("category")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        debug(f"user_prompt (first 100 chars): {user_prompt[:100]!r}")
        debug(f"target_os: {target_os}")
        debug(f"category: {category}")
        debug(f"ai_provider: {ai_provider}")
        debug(f"ai_settings: {ai_settings}")

        if not user_prompt:
            debug("ERROR: user_prompt is empty")
            return {
                "success": False,
                "error": "user_prompt is required",
            }

        try:
            debug("Getting AI shell command generator service...")
            service = self._get_ai_shell_command_generator_service()
            debug(f"Service obtained: {type(service)}")

            debug("Calling service.generate_command()...")
            result = service.generate_command(
                user_prompt=user_prompt,
                target_os=target_os,
                category=category,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
            debug(
                f"service.generate_command() returned: success={result.get('success')}, error={result.get('error')!r}"
            )
            debug(f"command (first 200 chars): {str(result.get('command', ''))[:200]!r}")

            return result

        except Exception as e:
            import traceback

            tb = traceback.format_exc()
            debug(f"EXCEPTION: {e}")
            debug(f"TRACEBACK:\n{tb}")

            return {
                "success": False,
                "error": f"AI shell command generation failed: {e}",
                "traceback": tb,
            }

    def _get_ai_builder_generator_service(self) -> AiBuilderGeneratorService:
        """Get or create the AI builder generator service."""
        if self._ai_builder_generator_service is None:
            self._ai_builder_generator_service = AiBuilderGeneratorService(
                event_manager=self.event_manager,
            )
        return self._ai_builder_generator_service  # type: ignore[no-any-return]

    def _handle_generate_context_with_ai(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle AI context generation for knowledge base entries."""
        user_prompt = params.get("user_prompt", "")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.generate_context(
                user_prompt=user_prompt,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Context generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_generate_api_request_with_ai(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle AI API request template generation."""
        user_prompt = params.get("user_prompt", "")
        base_url = params.get("base_url")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.generate_api_request(
                user_prompt=user_prompt,
                base_url=base_url,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"API request generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_generate_task_prompt_with_ai(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle AI task prompt generation/improvement."""
        user_prompt = params.get("user_prompt", "")
        mode = params.get("mode", "generate")
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.generate_task_prompt(
                user_prompt=user_prompt,
                mode=mode,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Task prompt generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_suggest_exploration_strategy_with_ai(
        self, params: dict[str, Any]
    ) -> dict[str, Any]:
        """Handle AI exploration strategy suggestion."""
        user_goal = params.get("user_goal", "")
        available_states = params.get("available_states", [])
        available_transitions = params.get("available_transitions", [])
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_goal:
            return {"success": False, "error": "user_goal is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.suggest_exploration_strategy(
                user_goal=user_goal,
                available_states=available_states,
                available_transitions=available_transitions,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Exploration strategy suggestion failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_generate_test_and_agentic_step(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle AI test and agentic step generation."""
        user_prompt = params.get("user_prompt", "")
        page_context = params.get("page_context")
        context_ids = params.get("context_ids")  # Context IDs from context library
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        # Fetch contexts if context_ids provided
        contexts_content = None
        if context_ids and len(context_ids) > 0:
            try:
                contexts_content = self._fetch_contexts_content(context_ids)
                self.event_manager.emit_log(
                    "info",
                    f"[GENERATE_TEST] Fetched {len(context_ids)} contexts for AI prompt",
                )
            except Exception as e:
                self.event_manager.emit_log(
                    "warning",
                    f"[GENERATE_TEST] Failed to fetch contexts: {e}",
                )

        try:
            service = self._get_ai_builder_generator_service()
            return service.generate_test_and_agentic_step(
                user_prompt=user_prompt,
                page_context=page_context,
                contexts_content=contexts_content,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Test and agentic step generation failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _handle_explore_flow_step(self, params: dict[str, Any]) -> dict[str, Any]:
        """Handle AI-driven flow exploration step.

        The AI analyzes current page elements and user's goal to determine
        what action to take next (click, type, wait, or done).
        """
        user_prompt = params.get("user_prompt", "")
        current_elements = params.get("current_elements", [])
        current_url = params.get("current_url", "")
        current_title = params.get("current_title", "")
        captured_pages = params.get("captured_pages", [])
        step_number = params.get("step_number", 1)
        ai_provider = params.get("ai_provider", "claude_cli")
        ai_settings = params.get("ai_settings", {})

        if not user_prompt:
            return {"success": False, "error": "user_prompt is required"}

        if not current_elements:
            return {"success": False, "error": "current_elements is required"}

        try:
            service = self._get_ai_builder_generator_service()
            return service.explore_flow_step(
                user_prompt=user_prompt,
                current_elements=current_elements,
                current_url=current_url,
                current_title=current_title,
                captured_pages=captured_pages,
                step_number=step_number,
                ai_provider=ai_provider,
                ai_settings=ai_settings,
            )
        except Exception as e:
            import traceback

            return {
                "success": False,
                "error": f"Flow exploration step failed: {e}",
                "traceback": traceback.format_exc(),
            }

    def _fetch_contexts_content(self, context_ids: list[str]) -> str | None:
        """Fetch context content from the runner HTTP API."""
        import requests

        contexts_parts = []
        for ctx_id in context_ids:
            try:
                response = requests.get(
                    f"{self._get_runner_api_base()}/contexts/{ctx_id}",
                    timeout=5,
                )
                if response.status_code == 200:
                    data = response.json()
                    if data.get("success") and data.get("data"):
                        ctx = data["data"]
                        name = ctx.get("name", "Unknown")
                        category = ctx.get("category", "")
                        content = ctx.get("content", "")
                        if content:
                            # Format as XML block (same as context injection elsewhere)
                            ctx_block = f'<context name="{name}"'
                            if category:
                                ctx_block += f' category="{category}"'
                            ctx_block += f">\n{content}\n</context>"
                            contexts_parts.append(ctx_block)
            except Exception as e:
                self.event_manager.emit_log(
                    "warning",
                    f"[FETCH_CONTEXT] Failed to fetch context {ctx_id}: {e}",
                )

        if contexts_parts:
            return "## Relevant Context\n\n" + "\n\n".join(contexts_parts)
        return None
