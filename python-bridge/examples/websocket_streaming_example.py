#!/usr/bin/env python3
"""
WebSocket Streaming Example

This script demonstrates how to use the WebSocket integration to stream
automation results to qontinui-web in real-time.

Prerequisites:
1. qontinui-web backend running on http://localhost:8000
2. Valid JWT token or email/password for authentication
3. Valid project UUID

Usage:
    python websocket_streaming_example.py
"""

import base64
import os
import sys
import time
from io import BytesIO
from pathlib import Path

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent))

from websocket_client import RunnerWebSocketClient, authenticate


def capture_fake_screenshot() -> str:
    """Generate a fake screenshot for demonstration."""
    try:
        from PIL import Image

        # Create a simple colored rectangle
        img = Image.new("RGB", (800, 600), color=(73, 109, 137))
        buffer = BytesIO()
        img.save(buffer, format="PNG")
        return base64.b64encode(buffer.getvalue()).decode("utf-8")
    except ImportError:
        # Return empty base64 if PIL not available
        return ""


async def main():
    """Main demonstration function."""

    # Configuration
    # Set these via environment variables or change the defaults
    API_URL = os.environ.get("QONTINUI_API_URL", "http://localhost:8000")
    WS_URL = os.environ.get("QONTINUI_WS_URL", "ws://localhost:8000")
    EMAIL = os.environ.get("QONTINUI_EMAIL", "")
    PASSWORD = os.environ.get("QONTINUI_PASSWORD", "")
    PROJECT_ID = os.environ.get("QONTINUI_PROJECT_ID", "")
    RUNNER_NAME = os.environ.get("QONTINUI_RUNNER_NAME", "Demo Runner")

    if not EMAIL or not PASSWORD:
        print("Error: Please set QONTINUI_EMAIL and QONTINUI_PASSWORD environment variables")
        print("Example:")
        print("  export QONTINUI_EMAIL=your-email@example.com")
        print("  export QONTINUI_PASSWORD=your-password")
        return

    if not PROJECT_ID:
        print("Error: Please set QONTINUI_PROJECT_ID environment variable")
        print("Example:")
        print("  export QONTINUI_PROJECT_ID=your-project-uuid-here")
        return

    print("=" * 60)
    print("WebSocket Streaming Demo")
    print("=" * 60)
    print()

    # Step 1: Authenticate
    print("[1/6] Authenticating with qontinui-web...")
    token = await authenticate(API_URL, EMAIL, PASSWORD)

    if not token:
        print("❌ Authentication failed. Check your credentials.")
        return

    print(f"✓ Authenticated successfully (token length: {len(token)})")
    print()

    # Step 2: Create WebSocket client
    print("[2/6] Creating WebSocket client...")
    client = RunnerWebSocketClient(
        api_url=WS_URL,
        token=token,
        project_id=PROJECT_ID,
        runner_name=RUNNER_NAME,
        auto_reconnect=True,
        heartbeat_interval=30,
    )
    print("✓ WebSocket client created")
    print()

    # Step 3: Connect to WebSocket server
    print("[3/6] Connecting to WebSocket server...")
    success = await client.connect()

    if not success:
        print("❌ Connection failed. Check server is running and URL is correct.")
        return

    print("✓ Connected to WebSocket server")
    print()

    # Step 4: Start automation session
    print("[4/6] Starting automation session...")
    config_snapshot = {
        "name": "Demo Workflow",
        "description": "Demonstrating WebSocket streaming",
        "version": "1.0.0",
    }
    success = await client.start_session(config_snapshot)

    if not success:
        print("❌ Failed to start session")
        await client.disconnect()
        return

    print(f"✓ Session started: {client.session_id}")
    print()

    # Step 5: Stream test events
    print("[5/6] Streaming test events...")
    print()

    # Event 1: Step started
    print("  → Sending step_started event...")
    await client.send_log(
        level="info",
        message="Step started: Login to application",
        log_data={
            "event_type": "step_started",
            "step_name": "Login to application",
            "step_type": "workflow",
            "timestamp": time.time(),
        },
    )
    await asyncio.sleep(1)

    # Event 2: Screenshot captured
    print("  → Sending screenshot...")
    screenshot_data = capture_fake_screenshot()
    if screenshot_data:
        screenshot_id = await client.send_screenshot(
            image=screenshot_data,
            name="login_page",
            metadata={
                "event_type": "screenshot_captured",
                "step_name": "Login to application",
                "state": "login_page",
            },
        )
        print(f"    ✓ Screenshot uploaded: {screenshot_id}")
    await asyncio.sleep(1)

    # Event 3: Action executed
    print("  → Sending action_completed event...")
    await client.send_log(
        level="info",
        message="Action completed: Click login button",
        log_data={
            "event_type": "action_completed",
            "action_type": "CLICK",
            "success": True,
            "timestamp": time.time(),
        },
    )
    await asyncio.sleep(1)

    # Event 4: Step completed
    print("  → Sending step_completed event...")
    await client.send_log(
        level="info",
        message="Step completed: Login to application",
        log_data={
            "event_type": "step_completed",
            "step_name": "Login to application",
            "success": True,
            "duration_ms": 3000,
            "timestamp": time.time(),
        },
    )
    await asyncio.sleep(1)

    # Event 5: Error example (optional)
    print("  → Sending error_detected event...")
    await client.send_log(
        level="error",
        message="Error detected: Unexpected popup appeared",
        log_data={
            "event_type": "error_detected",
            "error_message": "Unexpected popup appeared",
            "error_type": "ui_issue",
            "timestamp": time.time(),
        },
    )

    # Send error screenshot
    if screenshot_data:
        await client.send_screenshot(
            image=screenshot_data,
            name="error_popup",
            metadata={
                "event_type": "error_screenshot",
                "error_message": "Unexpected popup appeared",
            },
        )
    await asyncio.sleep(1)

    print()
    print("✓ Test events sent successfully")
    print()

    # Step 6: End session and disconnect
    print("[6/6] Ending session and disconnecting...")
    await client.end_session(status="completed")
    print("✓ Session ended")

    await client.disconnect()
    print("✓ Disconnected from WebSocket server")
    print()

    # Display statistics
    stats = client.get_stats()
    print("=" * 60)
    print("Session Statistics")
    print("=" * 60)
    print(f"Screenshots sent: {stats['screenshots_sent']}")
    print(f"Logs sent:        {stats['logs_sent']}")
    print(f"Heartbeats sent:  {stats['heartbeats_sent']}")
    print()

    print("✓ Demo completed successfully!")
    print()
    print("Check qontinui-web to see the streamed data:")
    print(f"  - Logs: http://localhost:3001/projects/{PROJECT_ID}/automation")
    print(f"  - Screenshots: http://localhost:3001/projects/{PROJECT_ID}/automation")
    print()


if __name__ == "__main__":
    import asyncio

    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n\n⚠️  Demo interrupted by user")
    except Exception as e:
        print(f"\n\n❌ Error: {e}")
        import traceback

        traceback.print_exc()
