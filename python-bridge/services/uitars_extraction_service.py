"""
UI-TARS Extraction Service

Provides GUI exploration using the UI-TARS vision-language model.
This service wraps the qontinui library's uitars module for use by the runner.
"""

import asyncio
import json
import threading
import time
import traceback
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any, Callable


class UITarsExtractionStatus(str, Enum):
    """Status of a UI-TARS extraction session."""
    IDLE = "idle"
    STARTING = "starting"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    STOPPED = "stopped"


@dataclass
class UITarsProgress:
    """Progress tracking for UI-TARS extraction."""
    status: UITarsExtractionStatus = UITarsExtractionStatus.IDLE
    current_step: int = 0
    max_steps: int = 50
    elapsed_seconds: float = 0.0
    last_thought: str | None = None
    last_action: str | None = None
    states_discovered: int = 0
    transitions_discovered: int = 0
    error_message: str | None = None


@dataclass
class UITarsExtractionConfig:
    """Configuration for UI-TARS extraction."""
    target_type: str  # "web" or "desktop"
    target: str  # URL for web, application name for desktop
    goal: str
    provider: str = "local_transformers"
    model_size: str = "2B"
    quantization: str = "int4"
    max_steps: int = 50
    timeout_seconds: int = 600
    save_screenshots: bool = True
    huggingface_endpoint: str | None = None
    huggingface_api_token: str | None = None
    vllm_server_url: str | None = None
    monitor_index: int = 0


@dataclass
class DiscoveredState:
    """A state discovered during exploration."""
    id: str
    name: str
    description: str
    screenshot_path: str
    elements: list[dict] = field(default_factory=list)


@dataclass
class DiscoveredTransition:
    """A transition discovered during exploration."""
    id: str
    from_state_id: str
    to_state_id: str
    action_type: str
    action_description: str
    coordinates: tuple[int, int] | None = None


class UITarsExtractionService:
    """
    Service for running UI-TARS exploration.

    This service manages the exploration lifecycle and communicates
    progress back to the runner.
    """

    def __init__(
        self,
        emit_log_fn: Callable[[str, str], None] | None = None,
        emit_event_fn: Callable[[str, dict], None] | None = None,
    ):
        self.emit_log = emit_log_fn or (lambda level, msg: None)
        self.emit_event = emit_event_fn or (lambda event, data: None)

        self._progress = UITarsProgress()
        self._config: UITarsExtractionConfig | None = None
        self._extraction_thread: threading.Thread | None = None
        self._stop_requested = False
        self._start_time: float | None = None

        # Results storage
        self._discovered_states: list[DiscoveredState] = []
        self._discovered_transitions: list[DiscoveredTransition] = []
        self._screenshots_dir: Path | None = None

        # Check if uitars is available
        self._uitars_available = False
        try:
            from qontinui.extraction.runtime.uitars import HAS_UITARS
            self._uitars_available = HAS_UITARS
        except ImportError:
            pass

    def is_available(self) -> bool:
        """Check if UI-TARS functionality is available."""
        return self._uitars_available

    def get_status(self) -> dict[str, Any]:
        """Get current extraction status."""
        return {
            "status": self._progress.status.value,
            "current_step": self._progress.current_step,
            "max_steps": self._progress.max_steps,
            "elapsed_seconds": self._progress.elapsed_seconds,
            "last_thought": self._progress.last_thought,
            "last_action": self._progress.last_action,
            "states_discovered": self._progress.states_discovered,
            "transitions_discovered": self._progress.transitions_discovered,
            "error_message": self._progress.error_message,
            "uitars_available": self._uitars_available,
        }

    def get_results(self) -> dict[str, Any]:
        """Get extraction results."""
        return {
            "states": [
                {
                    "id": s.id,
                    "name": s.name,
                    "description": s.description,
                    "screenshot_path": s.screenshot_path,
                    "elements": s.elements,
                }
                for s in self._discovered_states
            ],
            "transitions": [
                {
                    "id": t.id,
                    "from_state_id": t.from_state_id,
                    "to_state_id": t.to_state_id,
                    "action_type": t.action_type,
                    "action_description": t.action_description,
                    "coordinates": t.coordinates,
                }
                for t in self._discovered_transitions
            ],
            "total_steps": self._progress.current_step,
            "total_screenshots": len(self._discovered_states),
            "exploration_time_seconds": self._progress.elapsed_seconds,
        }

    def start_extraction(self, config: dict[str, Any]) -> dict[str, Any]:
        """Start a new UI-TARS extraction session."""
        if self._progress.status == UITarsExtractionStatus.RUNNING:
            return {
                "success": False,
                "error": "Extraction already in progress",
            }

        # Parse config
        try:
            self._config = UITarsExtractionConfig(
                target_type=config.get("target_type", "desktop"),
                target=config.get("target", ""),
                goal=config.get("goal", ""),
                provider=config.get("provider", "local_transformers"),
                model_size=config.get("model_size", "2B"),
                quantization=config.get("quantization", "int4"),
                max_steps=config.get("max_steps", 50),
                timeout_seconds=config.get("timeout_seconds", 600),
                save_screenshots=config.get("save_screenshots", True),
                huggingface_endpoint=config.get("huggingface_endpoint"),
                huggingface_api_token=config.get("huggingface_api_token"),
                vllm_server_url=config.get("vllm_server_url"),
                monitor_index=config.get("monitor_index", 0),
            )
        except Exception as e:
            return {
                "success": False,
                "error": f"Invalid configuration: {e}",
            }

        # Reset state
        self._progress = UITarsProgress(
            status=UITarsExtractionStatus.STARTING,
            max_steps=self._config.max_steps,
        )
        self._discovered_states = []
        self._discovered_transitions = []
        self._stop_requested = False
        self._start_time = time.time()

        # Start extraction thread
        self._extraction_thread = threading.Thread(
            target=self._run_extraction,
            daemon=True,
        )
        self._extraction_thread.start()

        self.emit_log("info", f"[UI-TARS] Started extraction: {self._config.target}")
        self._emit_progress_event()

        return {
            "success": True,
            "message": "Extraction started",
        }

    def stop_extraction(self) -> dict[str, Any]:
        """Stop the current extraction."""
        if self._progress.status not in (
            UITarsExtractionStatus.STARTING,
            UITarsExtractionStatus.RUNNING,
        ):
            return {
                "success": False,
                "error": "No extraction in progress",
            }

        self._stop_requested = True
        self._progress.status = UITarsExtractionStatus.STOPPED
        self.emit_log("info", "[UI-TARS] Stop requested")
        self._emit_progress_event()

        return {
            "success": True,
            "message": "Stop requested",
        }

    def _run_extraction(self):
        """Run the extraction in a background thread."""
        try:
            self._progress.status = UITarsExtractionStatus.RUNNING
            self._emit_progress_event()

            if not self._uitars_available:
                # Simulated extraction for demo/testing
                self._run_simulated_extraction()
            else:
                # Real UI-TARS extraction
                self._run_real_extraction()

            if self._progress.status == UITarsExtractionStatus.RUNNING:
                self._progress.status = UITarsExtractionStatus.COMPLETED
                self._emit_progress_event()
                self.emit_log("info", "[UI-TARS] Extraction completed")

        except Exception as e:
            self._progress.status = UITarsExtractionStatus.FAILED
            self._progress.error_message = str(e)
            self._emit_progress_event()
            self.emit_log("error", f"[UI-TARS] Extraction failed: {e}")
            traceback.print_exc()

    def _run_simulated_extraction(self):
        """Run a simulated extraction for demo/testing."""
        self.emit_log("info", "[UI-TARS] Running simulated extraction (UI-TARS not available)")

        import uuid

        thoughts = [
            "Analyzing the initial screen to identify interactive elements...",
            "Looking for clickable buttons and links...",
            "Examining the navigation menu structure...",
            "Checking for dropdown menus and interactive elements...",
            "Analyzing form inputs and their states...",
            "Exploring submenu options...",
            "Identifying state transitions...",
            "Capturing screenshot of current state...",
        ]

        actions = [
            "click(450, 230)",
            "hover(320, 180)",
            "click(600, 400)",
            "scroll(down, 200)",
            "click(150, 90)",
            "type('test input')",
            "click(800, 550)",
        ]

        import random

        for step in range(1, self._config.max_steps + 1):
            if self._stop_requested:
                self.emit_log("info", "[UI-TARS] Extraction stopped by user")
                return

            # Update progress
            self._progress.current_step = step
            self._progress.elapsed_seconds = time.time() - self._start_time
            self._progress.last_thought = random.choice(thoughts)
            self._progress.last_action = random.choice(actions)

            # Randomly discover states and transitions
            if random.random() > 0.7:
                state_id = str(uuid.uuid4())
                self._discovered_states.append(DiscoveredState(
                    id=state_id,
                    name=f"State_{len(self._discovered_states) + 1}",
                    description=f"Discovered at step {step}",
                    screenshot_path=f"/tmp/uitars_state_{state_id}.png",
                ))
                self._progress.states_discovered = len(self._discovered_states)

            if random.random() > 0.6 and len(self._discovered_states) >= 2:
                self._discovered_transitions.append(DiscoveredTransition(
                    id=str(uuid.uuid4()),
                    from_state_id=self._discovered_states[-2].id if len(self._discovered_states) >= 2 else "",
                    to_state_id=self._discovered_states[-1].id if self._discovered_states else "",
                    action_type="click",
                    action_description=self._progress.last_action or "",
                ))
                self._progress.transitions_discovered = len(self._discovered_transitions)

            self._emit_progress_event()
            time.sleep(2)  # Simulate inference time

    def _run_real_extraction(self):
        """Run real UI-TARS extraction using the qontinui library."""
        self.emit_log("info", "[UI-TARS] Running real extraction with UI-TARS model")

        try:
            from qontinui.extraction.runtime.uitars import (
                UITARSSettings,
                UITARSExplorer,
                UITARSExplorationConfig,
                TrajectoryConverter,
            )

            # Create settings from config
            settings = UITARSSettings(
                provider=self._config.provider,
                model_size=self._config.model_size,
                quantization=self._config.quantization,
                huggingface_endpoint=self._config.huggingface_endpoint,
                huggingface_api_token=self._config.huggingface_api_token,
                vllm_server_url=self._config.vllm_server_url,
                max_exploration_steps=self._config.max_steps,
                exploration_timeout_seconds=self._config.timeout_seconds,
                save_screenshots=self._config.save_screenshots,
            )

            # Create explorer
            explorer = UITARSExplorer(settings)

            # Create exploration config
            exploration_config = UITARSExplorationConfig(
                goal=self._config.goal,
                max_steps=self._config.max_steps,
            )

            # Set up progress callback
            def progress_callback(step: int, thought: str, action: str):
                if self._stop_requested:
                    raise InterruptedError("Extraction stopped by user")

                self._progress.current_step = step
                self._progress.elapsed_seconds = time.time() - self._start_time
                self._progress.last_thought = thought
                self._progress.last_action = action
                self._emit_progress_event()

            # Run exploration
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                # Connect to target
                if self._config.target_type == "web":
                    # For web, we'd need to start a browser
                    # For now, just use screenshot-based exploration
                    pass

                # Run exploration with progress callback
                trajectory = loop.run_until_complete(
                    explorer.explore(
                        config=exploration_config,
                        progress_callback=progress_callback,
                    )
                )

                # Convert trajectory to states/transitions
                converter = TrajectoryConverter()
                result = converter.convert(trajectory)

                # Store results
                for state in result.states:
                    self._discovered_states.append(DiscoveredState(
                        id=state.id,
                        name=state.name,
                        description=state.description or "",
                        screenshot_path=state.screenshot_path or "",
                        elements=[],
                    ))

                for transition in result.transitions:
                    self._discovered_transitions.append(DiscoveredTransition(
                        id=transition.id,
                        from_state_id=transition.from_state_id,
                        to_state_id=transition.to_state_id,
                        action_type=transition.action_type,
                        action_description=transition.action_description or "",
                        coordinates=transition.coordinates,
                    ))

                self._progress.states_discovered = len(self._discovered_states)
                self._progress.transitions_discovered = len(self._discovered_transitions)

            finally:
                loop.close()

        except InterruptedError:
            self.emit_log("info", "[UI-TARS] Extraction interrupted")
        except Exception as e:
            self.emit_log("error", f"[UI-TARS] Real extraction error: {e}")
            traceback.print_exc()
            raise

    def _emit_progress_event(self):
        """Emit a progress event to the runner."""
        self.emit_event("uitars_progress", self.get_status())


# Singleton instance
_service_instance: UITarsExtractionService | None = None


def get_uitars_extraction_service(
    emit_log_fn: Callable[[str, str], None] | None = None,
    emit_event_fn: Callable[[str, dict], None] | None = None,
) -> UITarsExtractionService:
    """Get or create the UI-TARS extraction service singleton."""
    global _service_instance
    if _service_instance is None:
        _service_instance = UITarsExtractionService(emit_log_fn, emit_event_fn)
    return _service_instance
