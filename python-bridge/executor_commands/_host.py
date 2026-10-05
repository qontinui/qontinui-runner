"""The executor state the command mixins read through ``self``, for the type checker.

Every mixin subclasses ``ExecutorHost``. At run time it is plain ``object``, so it adds
nothing to the MRO. Under ``TYPE_CHECKING`` it declares the attributes and core methods
that ``QontinuiExecutor`` provides and the mixins use, with the types the executor gives
them. A mixin that starts reading another executor attribute adds it here.
"""

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from event_manager import EventManager
    from executor_core import ExecutorCore
    from services.ui_bridge_explorer_service import UIBridgeExplorerService
    from services.uitars_extraction_service import UITarsExtractionService

    class ExecutorHost:
        """Attributes and methods of ``QontinuiExecutor`` that the mixins use."""

        config: Any
        event_manager: EventManager
        executor_core: ExecutorCore
        input_monitor_service: Any

        # Interaction recording
        _interaction_recording_active: bool
        _interaction_session_id: str | None
        _interaction_start_time: float | None
        _interaction_fps: int

        # Click capture
        _click_capture_active: bool
        _click_capture_session_id: str | None
        _click_capture_start_time: float | None
        _click_capture_output_dir: str | None
        _click_capture_application_hint: str | None

        # Lazily created services
        _web_extraction_service: Any
        _vision_extraction_service: Any
        _playwright_collector_service: Any
        _test_analysis_service: Any
        _ai_test_generator_service: Any
        _ai_shell_command_generator_service: Any
        _ai_builder_generator_service: Any
        _integration_testing_service: Any
        _accessibility_capture_service: Any
        _uitars_extraction_service: UITarsExtractionService | None
        _ui_bridge_explorer_service: UIBridgeExplorerService | None

        # UI Bridge state machine
        _ui_bridge_runtime: Any
        _element_resolver: Any

        def _emit_event_wrapper(self, event_type: str, data: dict[str, Any]) -> None: ...

        def _get_or_create_async_loop(self) -> Any: ...

        @staticmethod
        def _get_runner_api_base() -> str: ...

        def _get_sm_persistence(self) -> Any: ...

else:
    ExecutorHost = object

__all__ = ["ExecutorHost"]
