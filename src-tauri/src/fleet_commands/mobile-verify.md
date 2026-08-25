# Mobile Verify Command

Capture the current state of the mobile app (screenshot + logs) for verification.

## Instructions

You are verifying the current state of the qontinui-mobile app running on an Android device or emulator.

### Step 1: Capture Current State

Run the mobile feedback script to capture screenshot and logs:

```bash
python qontinui-claude-config/scripts/mobile-feedback.py capture
```

### Step 2: Read the Captured Data

1. Read the latest screenshot:
   - Path: `.dev-logs\mobile\screenshots\latest.png`

2. Read the latest logs:
   - Path: `.dev-logs\mobile\logcat\latest.txt`

3. Read the capture summary:
   - Path: `.dev-logs\mobile\latest_capture.json`

### Step 3: Analyze

Based on the screenshot and logs:
1. Describe what you see in the screenshot
2. Check for any errors or warnings in the logs
3. Verify the expected behavior based on recent code changes
4. If there are issues, identify the root cause and suggest fixes

### Step 4: Report

Provide a concise summary:
- **Status**: Working / Has Issues
- **Screenshot**: What's displayed
- **Logs**: Any errors or warnings
- **Next Steps**: What to fix or what's working correctly
