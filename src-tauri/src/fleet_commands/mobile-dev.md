# Mobile Development Feedback Loop

Autonomous mobile development with automatic verification.

## Instructions

You are developing the qontinui-mobile app with an autonomous feedback loop. After making code changes, you will automatically verify the result on the device/emulator.

### Pre-requisites Check

First, verify the development environment is ready. `mobile-feedback.py` is
carried by the `command-scripts` skill; it needs Python 3 and Android `adb`.
Each shell starts empty, so every block that calls it defines `command_script`
first:

```bash
command_script() {  # print the path of helper $CS_REL, carried by the command-scripts skill
  local d="$PWD" c
  while :; do
    c="$d/.claude/skills/command-scripts/_scripts/$CS_REL"
    [ -f "$c" ] && { printf '%s\n' "$c"; return 0; }
    [ "$d" = / ] && break; d=$(dirname "$d")
  done
  c="$HOME/.claude/skills/command-scripts/_scripts/$CS_REL"
  [ -f "$c" ] && { printf '%s\n' "$c"; return 0; }
  echo "command-scripts: _scripts/$CS_REL not found in .claude/skills/command-scripts/ under $PWD, any parent of it, or $HOME -- the command-scripts skill is not provisioned here" >&2
  return 1
}
MF=$(CS_REL=mobile-feedback.py command_script) && python3 "$MF" devices
```

If no devices are connected:
1. For emulator: run the block above with `start-emulator` in place of `devices`
2. For physical device: Connect via USB and enable USB debugging

### Development Workflow

For each code change you make:

1. **Make the code change** using Edit/Write tools

2. **Wait for hot reload** (Expo automatically reloads on save, ~2-3 seconds)

3. **Capture and verify** the result: run the pre-requisites block with
   `capture` in place of `devices`. It prints the path of each file it saved.

4. **Read the screenshot** to see the current app state:
   - `.dev-logs/mobile/screenshots/latest.png`

5. **Read the logs** if there are errors:
   - `.dev-logs/mobile/logcat/latest.txt`

6. **Analyze and iterate**:
   - If the change works: Move to next task
   - If there are issues: Fix and repeat from step 1

### Autonomous Mode

When working autonomously:
- Do NOT ask the user to verify changes
- Always capture screenshot after changes
- Always read and analyze the screenshot yourself
- Fix issues without user intervention
- Only report final results or if you're blocked

### Debugging Tips

- Clear logs before testing: run the pre-requisites block with `clear-logs` in place of `devices`
- React Native errors appear in red in logcat
- Check for "ReactNativeJS" tag in logs for JS errors
- Metro bundler errors appear in the Expo terminal

### Files

Paths are relative to the directory the script ran in. Set `MOBILE_FEEDBACK_DIR`
to write somewhere else; it replaces `.dev-logs/mobile` below.

- Screenshots: `.dev-logs/mobile/screenshots/`
- Logs: `.dev-logs/mobile/logcat/`
- Latest capture: `.dev-logs/mobile/latest_capture.json`
