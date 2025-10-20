"""DSL action executor bridge.

Bridges DSL method calls to Qontinui action execution.
"""

import logging
from typing import Any

logger = logging.getLogger(__name__)


class DSLActionExecutor:
    """Action executor for DSL method calls.

    Provides a bridge between DSL method calls and Qontinui action execution.
    This allows DSL statements to invoke Qontinui actions through method call
    expressions.
    """

    def __init__(self, runner: Any):
        """Initialize DSL action executor.

        Args:
            runner: Qontinui JSONRunner instance
        """
        self.runner = runner

    def execute_method(self, object_name: str, method_name: str, arguments: list[Any]) -> Any:
        """Execute a method call.

        Args:
            object_name: Name of the object (e.g., "actions", "state")
            method_name: Name of the method to call
            arguments: Method arguments

        Returns:
            Method execution result

        Raises:
            Exception: If method execution fails
        """
        logger.debug(f"DSL method call: {object_name}.{method_name}({arguments})")

        # Route method calls to appropriate Qontinui components
        if object_name == "actions" or object_name == "action":
            return self._execute_action_method(method_name, arguments)
        elif object_name == "state":
            return self._execute_state_method(method_name, arguments)
        elif object_name == "logger" or object_name == "log":
            return self._execute_logger_method(method_name, arguments)
        else:
            logger.warning(f"Unknown object: {object_name}")
            return None

    def _execute_action_method(self, method_name: str, arguments: list[Any]) -> Any:
        """Execute action method.

        Args:
            method_name: Action method name
            arguments: Method arguments

        Returns:
            Action result
        """
        # Map DSL method names to Qontinui action types
        # This is a simplified mapping - extend as needed
        method_to_action = {
            "click": "CLICK",
            "type": "TYPE",
            "wait": "WAIT",
            "find": "FIND",
            "scroll": "SCROLL",
            "drag": "DRAG",
        }

        action_type = method_to_action.get(method_name)
        if not action_type:
            logger.warning(f"Unknown action method: {method_name}")
            return False

        # For now, log the action call
        # In a full implementation, this would create and execute Qontinui actions
        logger.info(f"Executing action: {action_type} with args {arguments}")
        return True

    def _execute_state_method(self, method_name: str, arguments: list[Any]) -> Any:
        """Execute state method.

        Args:
            method_name: State method name
            arguments: Method arguments

        Returns:
            State operation result
        """
        if method_name == "getCurrent":
            # Get current state from runner
            if self.runner.state_executor:
                return getattr(self.runner.state_executor, "current_state", None)
            return None
        elif method_name == "goTo":
            # Navigate to a state
            if arguments:
                state_id = arguments[0]
                logger.info(f"Navigating to state: {state_id}")
                # In full implementation, would call state transition logic
                return True
            return False
        else:
            logger.warning(f"Unknown state method: {method_name}")
            return None

    def _execute_logger_method(self, method_name: str, arguments: list[Any]) -> None:
        """Execute logger method.

        Args:
            method_name: Logger method name
            arguments: Method arguments
        """
        if method_name == "log" or method_name == "info":
            if arguments:
                logger.info(str(arguments[0]))
        elif method_name == "debug":
            if arguments:
                logger.debug(str(arguments[0]))
        elif method_name == "warning" or method_name == "warn":
            if arguments:
                logger.warning(str(arguments[0]))
        elif method_name == "error":
            if arguments:
                logger.error(str(arguments[0]))
        else:
            logger.warning(f"Unknown logger method: {method_name}")
