#!/usr/bin/env python3
"""
Mobile Feedback Loop for Qontinui Mobile Development

Captures screenshots and logs from Android device/emulator for AI analysis.
Similar to qontinui-runner's feedback loop but for mobile development.

Usage:
    python mobile-feedback.py screenshot          # Capture screenshot
    python mobile-feedback.py logs                # Capture recent logs
    python mobile-feedback.py capture             # Capture both
    python mobile-feedback.py watch               # Watch mode: capture on file changes
    python mobile-feedback.py devices             # List connected devices
    python mobile-feedback.py start-emulator      # Start the default emulator
"""

import subprocess
import sys
import os
import time
import json
from datetime import datetime
from pathlib import Path

# Output directory for feedback: $MOBILE_FEEDBACK_DIR when set, else
# `.dev-logs/mobile` under the CURRENT directory. Never derived from this file's
# own location: the script also ships as a render inside a skill directory
# (`.claude/skills/command-scripts/_scripts/`), where a path counted up from
# `__file__` lands inside the skills tree instead of the project.
MOBILE_LOGS = Path(os.environ.get("MOBILE_FEEDBACK_DIR") or Path.cwd() / ".dev-logs" / "mobile").resolve()
MOBILE_SCREENSHOTS = MOBILE_LOGS / "screenshots"
MOBILE_LOGCAT = MOBILE_LOGS / "logcat"

# Ensure directories exist
MOBILE_SCREENSHOTS.mkdir(parents=True, exist_ok=True)
MOBILE_LOGCAT.mkdir(parents=True, exist_ok=True)


def get_adb_path() -> str:
    """Find ADB executable path."""
    # Check if adb is in PATH
    import shutil
    adb_in_path = shutil.which("adb")
    if adb_in_path:
        return adb_in_path

    # Check default Android SDK locations
    sdk_locations = [
        Path(os.environ.get("ANDROID_HOME", "")) / "platform-tools" / "adb.exe",
        Path(os.environ.get("LOCALAPPDATA", "")) / "Android" / "Sdk" / "platform-tools" / "adb.exe",
        Path.home() / "AppData" / "Local" / "Android" / "Sdk" / "platform-tools" / "adb.exe",
    ]

    for sdk_path in sdk_locations:
        if sdk_path.exists():
            return str(sdk_path)

    return "adb"  # Fall back to PATH lookup


def get_emulator_path() -> str:
    """Find emulator executable path."""
    import shutil
    emulator_in_path = shutil.which("emulator")
    if emulator_in_path:
        return emulator_in_path

    sdk_locations = [
        Path(os.environ.get("ANDROID_HOME", "")) / "emulator" / "emulator.exe",
        Path(os.environ.get("LOCALAPPDATA", "")) / "Android" / "Sdk" / "emulator" / "emulator.exe",
        Path.home() / "AppData" / "Local" / "Android" / "Sdk" / "emulator" / "emulator.exe",
    ]

    for sdk_path in sdk_locations:
        if sdk_path.exists():
            return str(sdk_path)

    return "emulator"


ADB_PATH = get_adb_path()
EMULATOR_PATH = get_emulator_path()


# Wall-clock bounds on the child processes this script WAITS ON, named ONCE each
# so a timeout's message and the bound it exceeded cannot drift apart.
# Unbounded, none of these could TIME OUT -- they could only HANG. `adb` hangs
# routinely (a half-connected device, a server mid-restart, an emulator still
# booting), and an unbounded `adb` call in a feedback loop is a session that
# never returns and never says why.
ADB_TIMEOUT_S = 120
#: `emulator -list-avds` only reads the AVD directory, but the emulator binary
#: is heavy to start on a loaded box.
EMULATOR_LIST_TIMEOUT_S = 60


def run_adb(args: list[str], check: bool = True) -> subprocess.CompletedProcess:
    """Run an ADB command.

    A timeout is a MEASUREMENT FAILURE, not an empty result: returning an empty
    `CompletedProcess` here would flow into `get_devices()` as "no devices
    attached" and into `get_logs()` as "no log lines", both of which read as
    facts about the phone rather than about this box.
    """
    cmd = [ADB_PATH] + args
    try:
        return subprocess.run(
            cmd, capture_output=True, text=True, check=check,
            timeout=ADB_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired:
        print(
            f"ERROR: `adb {' '.join(args)}` did not answer within its "
            f"{ADB_TIMEOUT_S}s bound. That is UNKNOWN, not an empty answer -- "
            "adb is most likely wedged. Try `adb kill-server` and re-run."
        )
        sys.exit(1)
    except FileNotFoundError:
        print(f"ERROR: ADB not found at {ADB_PATH}. Please install Android Studio and run setup-android-path.ps1")
        sys.exit(1)


def get_devices() -> list[dict]:
    """Get list of connected devices/emulators."""
    result = run_adb(["devices", "-l"])
    devices = []
    for line in result.stdout.strip().split("\n")[1:]:
        if line.strip() and "device" in line:
            parts = line.split()
            device_id = parts[0]
            device_type = "emulator" if device_id.startswith("emulator") else "physical"
            model = ""
            for part in parts:
                if part.startswith("model:"):
                    model = part.split(":")[1]
            devices.append({
                "id": device_id,
                "type": device_type,
                "model": model
            })
    return devices


def capture_screenshot(device_id: str = None, suffix: str = "") -> Path:
    """Capture a screenshot from the device."""
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    filename = f"screenshot_{timestamp}{suffix}.png"
    output_path = MOBILE_SCREENSHOTS / filename

    # Capture to device temp, then pull
    device_path = "/sdcard/qontinui_screenshot.png"

    adb_args = []
    if device_id:
        adb_args = ["-s", device_id]

    run_adb(adb_args + ["shell", "screencap", "-p", device_path])
    run_adb(adb_args + ["pull", device_path, str(output_path)])
    run_adb(adb_args + ["shell", "rm", device_path], check=False)

    print(f"Screenshot saved: {output_path}")

    # Also save as 'latest' for easy access
    latest_path = MOBILE_SCREENSHOTS / "latest.png"
    if latest_path.exists():
        latest_path.unlink()
    output_path.link_to(latest_path) if os.name != 'nt' else __import__('shutil').copy(output_path, latest_path)

    return output_path


def capture_logs(device_id: str = None, lines: int = 500, filter_app: bool = True) -> Path:
    """Capture recent logcat output."""
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    filename = f"logcat_{timestamp}.txt"
    output_path = MOBILE_LOGCAT / filename

    adb_args = []
    if device_id:
        adb_args = ["-s", device_id]

    # Get recent logs, optionally filtered to our app
    if filter_app:
        # Filter for Expo/React Native logs
        result = run_adb(adb_args + ["logcat", "-d", "-t", str(lines), "ReactNative:V", "ReactNativeJS:V", "Expo:V", "*:S"], check=False)
        if not result.stdout.strip():
            # Fallback to all logs if no React Native logs found
            result = run_adb(adb_args + ["logcat", "-d", "-t", str(lines)])
    else:
        result = run_adb(adb_args + ["logcat", "-d", "-t", str(lines)])

    output_path.write_text(result.stdout)
    print(f"Logs saved: {output_path}")

    # Also save as 'latest' for easy access
    latest_path = MOBILE_LOGCAT / "latest.txt"
    latest_path.write_text(result.stdout)

    return output_path


def capture_all(device_id: str = None) -> dict:
    """Capture screenshot and logs."""
    screenshot_path = capture_screenshot(device_id)
    logs_path = capture_logs(device_id)

    # Write a summary JSON for AI consumption
    summary = {
        "timestamp": datetime.now().isoformat(),
        "device_id": device_id or "default",
        "screenshot": str(screenshot_path),
        "logs": str(logs_path),
    }

    summary_path = MOBILE_LOGS / "latest_capture.json"
    summary_path.write_text(json.dumps(summary, indent=2))

    print(f"\nCapture complete. Summary: {summary_path}")
    return summary


def list_emulators() -> list[str]:
    """List available AVDs (Android Virtual Devices)."""
    try:
        result = subprocess.run(
            [EMULATOR_PATH, "-list-avds"], capture_output=True, text=True,
            timeout=EMULATOR_LIST_TIMEOUT_S,
        )
        avds = [line.strip() for line in result.stdout.strip().split("\n") if line.strip()]
        return avds
    except subprocess.TimeoutExpired:
        # `[]` is this function's "no AVDs are defined", and `start_emulator`
        # prints exactly that advice ("Create one in Android Studio"). A timeout
        # must not be answered with instructions to create something that
        # already exists.
        print(
            f"ERROR: `emulator -list-avds` did not answer within its "
            f"{EMULATOR_LIST_TIMEOUT_S}s bound, so the AVD list is UNKNOWN -- "
            "not empty."
        )
        return []
    except FileNotFoundError:
        print(f"ERROR: Emulator not found at {EMULATOR_PATH}. Please install Android Studio and run setup-android-path.ps1")
        return []


def start_emulator(avd_name: str = None) -> bool:
    """Start an Android emulator."""
    avds = list_emulators()

    if not avds:
        print("No AVDs found. Create one in Android Studio: Tools > Device Manager")
        return False

    if avd_name is None:
        avd_name = avds[0]
        print(f"Starting default emulator: {avd_name}")
    elif avd_name not in avds:
        print(f"AVD '{avd_name}' not found. Available: {avds}")
        return False

    # Start emulator in background. DELIBERATELY UNBOUNDED: this child is meant
    # to outlive this script, and `Popen` takes no `timeout=` -- the bound would
    # have to be on a `.wait()` that must never happen here. The boot wait below
    # is the bound that actually applies, and it is a poll loop with its own
    # ceiling rather than a wait on this process.
    subprocess.Popen(
        [EMULATOR_PATH, "-avd", avd_name],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if os.name == 'nt' else 0
    )

    print(f"Emulator '{avd_name}' starting... (this takes a moment)")

    # Wait for device to be ready
    print("Waiting for device to boot...")
    for _ in range(60):
        time.sleep(2)
        devices = get_devices()
        emulators = [d for d in devices if d["type"] == "emulator"]
        if emulators:
            # Check if boot completed
            result = run_adb(["-s", emulators[0]["id"], "shell", "getprop", "sys.boot_completed"], check=False)
            if result.stdout.strip() == "1":
                print(f"Emulator ready: {emulators[0]['id']}")
                return True

    print("Timeout waiting for emulator to boot")
    return False


def watch_mode(device_id: str = None, interval: int = 5):
    """Watch for changes and capture periodically."""
    print(f"Watch mode started. Capturing every {interval} seconds. Press Ctrl+C to stop.")

    try:
        while True:
            capture_all(device_id)
            time.sleep(interval)
    except KeyboardInterrupt:
        print("\nWatch mode stopped.")


def clear_logs():
    """Clear the logcat buffer."""
    run_adb(["logcat", "-c"])
    print("Logcat buffer cleared.")


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(0)

    command = sys.argv[1]
    device_id = None

    # Check for -s device flag
    if "-s" in sys.argv:
        idx = sys.argv.index("-s")
        if idx + 1 < len(sys.argv):
            device_id = sys.argv[idx + 1]

    if command == "devices":
        devices = get_devices()
        if devices:
            print("Connected devices:")
            for d in devices:
                print(f"  {d['id']} ({d['type']}) {d['model']}")
        else:
            print("No devices connected.")

        avds = list_emulators()
        if avds:
            print("\nAvailable emulators (AVDs):")
            for avd in avds:
                print(f"  {avd}")

    elif command == "screenshot":
        capture_screenshot(device_id)

    elif command == "logs":
        capture_logs(device_id)

    elif command == "capture":
        capture_all(device_id)

    elif command == "watch":
        interval = int(sys.argv[2]) if len(sys.argv) > 2 else 5
        watch_mode(device_id, interval)

    elif command == "start-emulator":
        avd_name = sys.argv[2] if len(sys.argv) > 2 else None
        start_emulator(avd_name)

    elif command == "clear-logs":
        clear_logs()

    else:
        print(f"Unknown command: {command}")
        print(__doc__)
        sys.exit(1)


if __name__ == "__main__":
    main()
