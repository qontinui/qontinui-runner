## Input Validation: Debugging Click Position Issues

When "Capture Input for Validation" is enabled in the Advanced settings, the runner records actual mouse events and compares them to reported click positions. This helps debug coordinate calculation bugs.

### When to Use This Feature

Enable input validation when clicks are:

- Missing their targets despite correct image recognition
- Landing in wrong locations on multi-monitor setups
- Offset due to DPI scaling issues
- Affected by coordinate transformation bugs

### Understanding the Data

When enabled, the Iteration Bundle includes an **Input Validation** section with:

1. **Summary** - Overview of discrepancies found
   - Total clicks vs captured clicks
   - Number of significant discrepancies
   - Max/average offset in pixels

2. **Click Position Comparisons Table**
   - **Reported**: Where the automation engine said it would click
   - **Actual**: Where the mouse actually clicked (captured by input monitor)
   - **Δ px**: Distance between reported and actual positions
   - **Status**: ✅ OK, ⚠️ OFFSET, or ❓ N/A

3. **Highlighted Discrepancies** - Details on each significant offset

### Interpreting Results

| Discrepancy                                | Likely Cause                     |
| ------------------------------------------ | -------------------------------- |
| Consistent offset (e.g., always +1920px X) | Multi-monitor coordinate issue   |
| Scaled offset (e.g., 1.5x or 2x)           | DPI scaling not applied          |
| Random small offsets (< 5px)               | Normal - click targets still hit |
| Large variable offsets                     | Coordinate transformation bug    |

### Common Fixes

**Multi-monitor offset**: Check that monitor bounds are correctly calculated and the target monitor index is correct.

**DPI scaling**: Ensure coordinates are scaled by the display's scale factor before clicking.

**Wrong monitor**: Verify the image recognition is searching the correct monitor and coordinates are relative to that monitor.

### Raw Events File

The raw input events are saved to `.dev-logs/input_events/{session_id}_events.jsonl` and include:

- All mouse clicks with exact timestamps and positions
- Mouse movements (sampled)
- Keyboard events
- Frame numbers for video correlation
