"""HTTP client for the qontinui-runner API (port 9876)."""

from __future__ import annotations

import logging
import os
import platform
from dataclasses import dataclass, field
from typing import Any

import httpx

logger = logging.getLogger(__name__)

DEFAULT_HOST = "localhost"
DEFAULT_PORT = 9876
DEFAULT_TIMEOUT = 30.0


def _get_windows_host() -> str:
    """Get the Windows host IP when running inside WSL."""
    try:
        with open("/etc/resolv.conf") as f:
            for line in f:
                if line.startswith("nameserver"):
                    return line.split()[1]
    except (FileNotFoundError, IndexError):
        pass
    return DEFAULT_HOST


def _detect_host() -> str:
    """Auto-detect the correct host for reaching the runner."""
    env_host = os.environ.get("QONTINUI_RUNNER_HOST")
    if env_host:
        return env_host
    if platform.system() == "Linux" and os.path.exists("/proc/version"):
        try:
            with open("/proc/version") as f:
                if "microsoft" in f.read().lower():
                    return _get_windows_host()
        except OSError:
            pass
    return DEFAULT_HOST


@dataclass
class RunnerResponse:
    """Wrapper for runner API responses."""

    success: bool
    data: Any = None
    error: str | None = None


@dataclass
class RunnerClient:
    """Async HTTP client for the qontinui-runner settings API."""

    host: str = field(default_factory=_detect_host)
    port: int = field(
        default_factory=lambda: int(
            os.environ.get("QONTINUI_RUNNER_PORT", str(DEFAULT_PORT))
        )
    )
    _client: httpx.AsyncClient | None = field(default=None, init=False, repr=False)

    @property
    def base_url(self) -> str:
        return f"http://{self.host}:{self.port}"

    async def _get_client(self) -> httpx.AsyncClient:
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                base_url=self.base_url, timeout=DEFAULT_TIMEOUT
            )
        return self._client

    async def close(self) -> None:
        if self._client and not self._client.is_closed:
            await self._client.aclose()
            self._client = None

    async def _request(
        self,
        method: str,
        path: str,
        json: Any | None = None,
        timeout: float = DEFAULT_TIMEOUT,
    ) -> RunnerResponse:
        """Make an HTTP request to the runner API."""
        client = await self._get_client()
        try:
            response = await client.request(method, path, json=json, timeout=timeout)
            response.raise_for_status()
            body = response.json()
            return RunnerResponse(
                success=body.get("success", True),
                data=body.get("data"),
                error=body.get("error"),
            )
        except httpx.ConnectError:
            return RunnerResponse(
                success=False,
                error=f"Cannot connect to runner at {self.base_url}. Is it running?",
            )
        except httpx.HTTPStatusError as e:
            try:
                body = e.response.json()
                return RunnerResponse(
                    success=False,
                    error=body.get("error", str(e)),
                )
            except Exception:
                return RunnerResponse(success=False, error=str(e))
        except Exception as e:
            return RunnerResponse(success=False, error=str(e))

    # ── Health ──────────────────────────────────────────────────────────

    async def health(self) -> RunnerResponse:
        return await self._request("GET", "/health")

    # ── Log Sources ─────────────────────────────────────────────────────

    async def get_log_source_settings(self) -> RunnerResponse:
        return await self._request("GET", "/log-sources/settings")

    async def put_log_source_settings(self, settings: dict[str, Any]) -> RunnerResponse:
        return await self._request("PUT", "/log-sources/settings", json=settings)

    async def set_ai_selection_mode(self, mode: str) -> RunnerResponse:
        return await self._request("PUT", "/log-sources/ai-mode", json={"mode": mode})

    async def set_default_profile(self, profile_id: str | None) -> RunnerResponse:
        return await self._request(
            "PUT", "/log-sources/default-profile", json={"profile_id": profile_id}
        )

    # ── AI Settings ─────────────────────────────────────────────────────

    async def get_ai_settings(self) -> RunnerResponse:
        return await self._request("GET", "/settings/ai")

    async def put_ai_settings(self, settings: dict[str, Any]) -> RunnerResponse:
        return await self._request("PUT", "/settings/ai", json=settings)

    async def store_api_key(self, provider: str, api_key: str) -> RunnerResponse:
        return await self._request(
            "POST",
            "/settings/ai/api-key",
            json={"provider": provider, "api_key": api_key},
        )

    async def check_api_key(self, provider: str) -> RunnerResponse:
        return await self._request("GET", f"/settings/ai/has-key/{provider}")

    async def test_ai_connection(self) -> RunnerResponse:
        return await self._request("POST", "/settings/ai/test-connection")

    # ── Device Info ─────────────────────────────────────────────────────

    async def get_device_info(self) -> RunnerResponse:
        return await self._request("GET", "/settings/device-info")
