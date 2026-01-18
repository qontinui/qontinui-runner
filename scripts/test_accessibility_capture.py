#!/usr/bin/env python3
"""Manual test script for accessibility capture.

This script demonstrates the accessibility capture functionality:
1. Connects to Chrome via CDP
2. Captures the accessibility tree
3. Prints the AI context
4. Optionally clicks a specific ref
5. Verifies the action worked

Prerequisites:
    - Chrome/Chromium running with remote debugging enabled:
        chrome --remote-debugging-port=9222

Usage:
    python test_accessibility_capture.py
    python test_accessibility_capture.py --port 9333
    python test_accessibility_capture.py --click @e5
    python test_accessibility_capture.py --type "@e2=hello@example.com"
"""

import argparse
import asyncio
import sys
from pathlib import Path

# Add parent paths for imports
_script_path = Path(__file__).resolve()
_runner_path = _script_path.parent.parent
_parent_path = _runner_path.parent

# Add qontinui library to path
sys.path.insert(0, str(_parent_path / "qontinui" / "src"))
sys.path.insert(0, str(_parent_path / "qontinui-schemas" / "src"))

from qontinui_schemas.accessibility import (
    AccessibilityBackend,
    AccessibilityCaptureOptions,
    AccessibilityConfig,
    AccessibilityRole,
    AccessibilitySelector,
)

from qontinui.hal.implementations.accessibility.cdp_capture import CDPAccessibilityCapture


def print_header(text: str) -> None:
    """Print a section header."""
    print()
    print("=" * 60)
    print(f"  {text}")
    print("=" * 60)


def print_node_tree(node, depth: int = 0, max_depth: int = 3) -> None:
    """Print a simplified view of the node tree."""
    if depth > max_depth:
        return

    indent = "  " * depth
    name_str = f' "{node.name}"' if node.name else ""
    interactive_str = " [interactive]" if node.is_interactive else ""
    print(f"{indent}{node.ref}: {node.role.value}{name_str}{interactive_str}")

    for child in node.children[:5]:  # Limit children shown
        print_node_tree(child, depth + 1, max_depth)

    if len(node.children) > 5:
        print(f"{indent}  ... ({len(node.children) - 5} more children)")


async def main(args: argparse.Namespace) -> int:
    """Main entry point."""
    print_header("Accessibility Capture Test")

    # Create capture with config
    config = AccessibilityConfig(
        backend=AccessibilityBackend.CDP,
        cdp_host=args.host,
        cdp_port=args.port,
        cdp_timeout=args.timeout,
        interactive_only=args.interactive_only,
        include_hidden=False,
        include_bounds=True,
        include_value=True,
    )

    capture = CDPAccessibilityCapture(config=config)

    try:
        # Step 1: Connect
        print_header("Step 1: Connecting to CDP")
        print(f"Host: {args.host}")
        print(f"Port: {args.port}")

        connected = await capture.connect(host=args.host, port=args.port, timeout=args.timeout)

        if not connected:
            print()
            print("ERROR: Failed to connect to Chrome via CDP")
            print()
            print("Make sure Chrome is running with remote debugging enabled:")
            print(f"  chrome --remote-debugging-port={args.port}")
            return 1

        print("SUCCESS: Connected to CDP")

        # Step 2: Capture tree
        print_header("Step 2: Capturing Accessibility Tree")

        options = AccessibilityCaptureOptions(
            target="auto",
            include_screenshot=False,
            config=config,
        )
        snapshot = await capture.capture_tree(options=options)

        print(f"Backend: {snapshot.backend.value}")
        print(f"URL: {snapshot.url or 'N/A'}")
        print(f"Title: {snapshot.title or 'N/A'}")
        print(f"Total nodes: {snapshot.total_nodes}")
        print(f"Interactive nodes: {snapshot.interactive_nodes}")
        print(f"Timestamp: {snapshot.timestamp}")

        # Step 3: Print tree preview
        print_header("Step 3: Tree Preview (first 3 levels)")
        print_node_tree(snapshot.root, max_depth=3)

        # Step 4: Print AI context
        print_header("Step 4: AI Context")
        context = capture.to_ai_context(
            snapshot,
            max_elements=args.max_elements,
            interactive_only=True,
        )
        print(context)

        # Step 5: Find specific elements (optional)
        if args.find_role:
            print_header(f"Step 5: Finding elements with role '{args.find_role}'")
            try:
                role = AccessibilityRole(args.find_role)
                selector = AccessibilitySelector(role=role)
                nodes = await capture.find_nodes(selector)
                print(f"Found {len(nodes)} element(s):")
                for node in nodes[:10]:
                    print(f"  {node.ref}: {node.role.value} '{node.name or ''}'")
                if len(nodes) > 10:
                    print(f"  ... ({len(nodes) - 10} more)")
            except ValueError:
                print(f"Unknown role: {args.find_role}")

        # Step 6: Click by ref (optional)
        if args.click:
            print_header(f"Step 6: Clicking ref '{args.click}'")
            node = await capture.get_node_by_ref(args.click)
            if node:
                print(f"Found: {node.role.value} '{node.name or ''}'")
                success = await capture.click_by_ref(args.click)
                print(f"Click {'succeeded' if success else 'failed'}")
            else:
                print(f"ERROR: Ref '{args.click}' not found")

        # Step 7: Type by ref (optional)
        if args.type:
            ref, text = args.type.split("=", 1) if "=" in args.type else (args.type, "test text")
            print_header(f"Step 7: Typing into ref '{ref}'")
            node = await capture.get_node_by_ref(ref)
            if node:
                print(f"Found: {node.role.value} '{node.name or ''}'")
                print(f"Typing: {text}")
                success = await capture.type_by_ref(ref, text)
                print(f"Type {'succeeded' if success else 'failed'}")
            else:
                print(f"ERROR: Ref '{ref}' not found")

        print_header("Test Complete")
        print("SUCCESS: All operations completed")
        return 0

    except Exception as e:
        print()
        print(f"ERROR: {type(e).__name__}: {e}")
        import traceback
        traceback.print_exc()
        return 1

    finally:
        await capture.disconnect()
        print()
        print("Disconnected from CDP")


def parse_args() -> argparse.Namespace:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        description="Test accessibility capture via CDP",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    # Basic capture and display
    python test_accessibility_capture.py

    # Connect to different port
    python test_accessibility_capture.py --port 9333

    # Click an element
    python test_accessibility_capture.py --click @e5

    # Type into an element
    python test_accessibility_capture.py --type "@e2=hello@example.com"

    # Find all buttons
    python test_accessibility_capture.py --find-role button

    # Show only interactive elements
    python test_accessibility_capture.py --interactive-only
""",
    )

    parser.add_argument(
        "--host",
        default="localhost",
        help="CDP host (default: localhost)",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=9222,
        help="CDP port (default: 9222)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=10.0,
        help="Connection timeout in seconds (default: 10)",
    )
    parser.add_argument(
        "--interactive-only",
        action="store_true",
        help="Only capture interactive elements",
    )
    parser.add_argument(
        "--max-elements",
        type=int,
        default=50,
        help="Maximum elements in AI context (default: 50)",
    )
    parser.add_argument(
        "--find-role",
        help="Find elements with this role (e.g., button, textbox)",
    )
    parser.add_argument(
        "--click",
        help="Click element with this ref (e.g., @e5)",
    )
    parser.add_argument(
        "--type",
        help="Type into element (format: ref=text, e.g., @e2=hello)",
    )

    return parser.parse_args()


if __name__ == "__main__":
    args = parse_args()
    exit_code = asyncio.run(main(args))
    sys.exit(exit_code)
