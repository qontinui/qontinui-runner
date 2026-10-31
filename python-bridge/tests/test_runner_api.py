"""runner_api: every runner URL the bridge builds names 127.0.0.1 and QONTINUI_PORT.

Literal expectations, so a regression to ``localhost`` (which pays a ~2 s failed
``::1`` connect per new connection on Windows) reds this file. Plan
``2026-10-25-runner-self-calls-dial-localhost-and-pay-a-doomed-ipv6-connect``.
"""

import runner_api
from ui_bridge_http_client import UIBridgeHTTPClient


def test_base_defaults_to_127_0_0_1_9876(monkeypatch):
    monkeypatch.delenv("QONTINUI_PORT", raising=False)
    assert runner_api.runner_api_port() == 9876
    assert runner_api.runner_api_base() == "http://127.0.0.1:9876"


def test_base_follows_qontinui_port(monkeypatch):
    monkeypatch.setenv("QONTINUI_PORT", "9877")
    assert runner_api.runner_api_port() == 9877
    assert runner_api.runner_api_base() == "http://127.0.0.1:9877"


def test_http_client_default_resolves_at_call_time(monkeypatch):
    monkeypatch.setenv("QONTINUI_PORT", "9878")
    assert UIBridgeHTTPClient().base_url == "http://127.0.0.1:9878"
    assert UIBridgeHTTPClient("http://127.0.0.1:9999/").base_url == "http://127.0.0.1:9999"
