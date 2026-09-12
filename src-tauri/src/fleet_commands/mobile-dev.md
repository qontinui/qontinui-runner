# Mobile Development Feedback Loop

Autonomous mobile development with automatic verification.

## Instructions

You are developing the qontinui-mobile app with an autonomous feedback loop. After making code changes, you will automatically verify the result on the device/emulator.

### Pre-requisites Check

First, verify the development environment is ready:

```bash
python qontinui-claude-config/scripts/mobile-feedback.py devices
```

If no devices are connected:
1. For emulator: `python qontinui-claude-config/scripts/mobile-feedback.py start-emulator`
2. For physical device: Connect via USB and enable USB debugging

### Development Workflow

For each code change you make:

1. **Make the code change** using Edit/Write tools

2. **Wait for hot reload** (Expo automatically reloads on save, ~2-3 seconds)

3. **Capture and verify** the result:
   ```bash
   python qontinui-claude-config/scripts/mobile-feedback.py capture
   ```

4. **Read the screenshot** to see the current app state:
   - `.dev-logs\mobile\screenshots\latest.png`

5. **Read the logs** if there are errors:
   - `.dev-logs\mobile\logcat\latest.txt`

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

- Clear logs before testing: `python mobile-feedback.py clear-logs`
- React Native errors appear in red in logcat
- Check for "ReactNativeJS" tag in logs for JS errors
- Metro bundler errors appear in the Expo terminal

### Files

- Screenshots: `.dev-logs/mobile/screenshots/`
- Logs: `.dev-logs/mobile/logcat/`
- Latest capture: `.dev-logs/mobile/latest_capture.json`
