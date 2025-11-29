"""
WebSocket configuration for qontinui-runner.

Manages settings for connecting to qontinui-web backend.
"""

import os
from typing import Optional
from dataclasses import dataclass


@dataclass
class WebSocketConfig:
    """Configuration for WebSocket client."""

    # WebSocket endpoint
    api_url: str = "ws://localhost:8001"

    # Authentication
    email: Optional[str] = None
    password: Optional[str] = None
    token: Optional[str] = None

    # Project
    project_id: Optional[str] = None

    # Connection settings
    enabled: bool = False
    auto_reconnect: bool = True
    heartbeat_interval: int = 30
    max_reconnect_attempts: int = 5

    # Runner info
    runner_version: str = "0.1.0"
    runner_name: Optional[str] = None  # Custom user-defined runner name

    @classmethod
    def from_env(cls) -> "WebSocketConfig":
        """
        Load configuration from environment variables.

        Environment variables:
        - QONTINUI_WS_ENABLED: Enable WebSocket (true/false)
        - QONTINUI_WS_URL: WebSocket API URL (default: ws://localhost:8001)
        - QONTINUI_WS_EMAIL: User email for authentication
        - QONTINUI_WS_PASSWORD: User password for authentication
        - QONTINUI_WS_TOKEN: JWT token (if already authenticated)
        - QONTINUI_WS_PROJECT_ID: Project UUID
        - QONTINUI_WS_HEARTBEAT_INTERVAL: Heartbeat interval in seconds (default: 30)
        - QONTINUI_RUNNER_NAME: Custom name for this runner (e.g., "My Laptop")

        Returns:
            WebSocketConfig instance
        """
        return cls(
            enabled=os.getenv("QONTINUI_WS_ENABLED", "false").lower() == "true",
            api_url=os.getenv("QONTINUI_WS_URL", "ws://localhost:8001"),
            email=os.getenv("QONTINUI_WS_EMAIL"),
            password=os.getenv("QONTINUI_WS_PASSWORD"),
            token=os.getenv("QONTINUI_WS_TOKEN"),
            project_id=os.getenv("QONTINUI_WS_PROJECT_ID"),
            heartbeat_interval=int(os.getenv("QONTINUI_WS_HEARTBEAT_INTERVAL", "30")),
            auto_reconnect=os.getenv("QONTINUI_WS_AUTO_RECONNECT", "true").lower() == "true",
            max_reconnect_attempts=int(os.getenv("QONTINUI_WS_MAX_RECONNECT", "5")),
            runner_version=os.getenv("QONTINUI_RUNNER_VERSION", "0.1.0"),
            runner_name=os.getenv("QONTINUI_RUNNER_NAME"),
        )

    @classmethod
    def from_dict(cls, data: dict) -> "WebSocketConfig":
        """
        Load configuration from dictionary.

        Args:
            data: Configuration dictionary

        Returns:
            WebSocketConfig instance
        """
        return cls(
            enabled=data.get("enabled", False),
            api_url=data.get("api_url", "ws://localhost:8001"),
            email=data.get("email"),
            password=data.get("password"),
            token=data.get("token"),
            project_id=data.get("project_id"),
            heartbeat_interval=data.get("heartbeat_interval", 30),
            auto_reconnect=data.get("auto_reconnect", True),
            max_reconnect_attempts=data.get("max_reconnect_attempts", 5),
            runner_version=data.get("runner_version", "0.1.0"),
            runner_name=data.get("runner_name"),
        )

    def validate(self) -> tuple[bool, Optional[str]]:
        """
        Validate configuration.

        Returns:
            Tuple of (is_valid, error_message)
        """
        if not self.enabled:
            return True, None

        if not self.api_url:
            return False, "WebSocket API URL is required"

        if not self.token and not (self.email and self.password):
            return False, "Either token or email/password is required for authentication"

        # project_id is optional - runner can connect without a project
        # and projects are assigned later via the web UI

        if self.heartbeat_interval < 10:
            return False, "Heartbeat interval must be at least 10 seconds"

        return True, None

    def to_dict(self) -> dict:
        """
        Convert configuration to dictionary.

        Returns:
            Configuration dictionary
        """
        return {
            "enabled": self.enabled,
            "api_url": self.api_url,
            "email": self.email,
            "password": "***" if self.password else None,  # Mask password
            "token": "***" if self.token else None,  # Mask token
            "project_id": self.project_id,
            "heartbeat_interval": self.heartbeat_interval,
            "auto_reconnect": self.auto_reconnect,
            "max_reconnect_attempts": self.max_reconnect_attempts,
            "runner_version": self.runner_version,
            "runner_name": self.runner_name,
        }
