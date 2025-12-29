# Verify Findings Recording

You are verifying that the FindingsTracker in qontinui-runner correctly parses and records findings from AI output.

## Your Task

1. **Read Recent AI Output**
   - Read the file `.dev-logs/ai-output.jsonl` to get the AI output from recent sessions
   - Focus on the most recent session (last ~100 lines should be sufficient)

2. **Extract Expected Findings**
   - Look for `[FINDING:category:severity]` markers in the AI output
   - Each finding block ends with `[/FINDING]`
   - Parse the category, severity, and content of each finding
   - Build a list of "expected findings" that SHOULD have been recorded

3. **Query Recorded Findings**
   - Call the findings API: `Invoke-WebRequest -Uri "http://localhost:9876/findings" | ConvertFrom-Json`
   - This returns what was actually recorded by the FindingsTracker

4. **Compare Expected vs Actual**
   - Check if each expected finding was recorded
   - Check for false positives (recorded but not in AI output)
   - Report any discrepancies

5. **If Discrepancies Found**
   - Investigate the parsing logic in `qontinui-runner/src/services/FindingsTracker.ts`
   - Check the regex patterns at the top of the file:
     - `FINDING_START_PATTERN`
     - `FINDING_END_PATTERN`
     - `TITLE_PATTERN`, `DESCRIPTION_PATTERN`, etc.
   - Fix any bugs in the parsing logic
   - The finding format is:
     ```
     [FINDING:category_id:severity]
     Title: Brief title
     Description: What the issue is
     File: path/to/file.ts (optional)
     Line: 42 (optional)
     Resolution: What was done (optional)
     [/FINDING]
     ```

## Finding Categories

Valid categories: `code_bug`, `security`, `todo`, `enhancement`, `already_fixed`, `warning`, `documentation`, `performance`, `test_issue`

Valid severities: `critical`, `high`, `medium`, `low`, `info`

## Key Files to Investigate

- `qontinui-runner/src/services/FindingsTracker.ts` - Main parsing logic
- `qontinui-runner/src/types/findings.ts` - Type definitions
- `qontinui-runner/src/services/FindingCategories.ts` - Category definitions

## Expected Output

Report your findings in this format:

```
## Findings Verification Report

### AI Output Analyzed
- Session: [session id or timestamp]
- Lines analyzed: X
- Finding markers found: Y

### Expected Findings
1. [category:severity] Title...
2. [category:severity] Title...

### Recorded Findings
1. [category:severity] Title...
2. [category:severity] Title...

### Discrepancies
- Missing: [list any findings that should have been recorded but weren't]
- Extra: [list any findings recorded but not in AI output]
- Malformed: [list any findings that were partially parsed]

### Root Cause (if discrepancies found)
[Explain what's wrong with the parsing logic]

### Fix Applied
[Describe the fix you made to FindingsTracker.ts]
```

## Important Notes

- The FindingsTracker uses regex to parse findings as they stream in from Claude
- Multi-line content (like Description) uses lookahead to find the boundary
- The tracker maintains a parsing buffer for handling findings that span multiple chunks
- If no discrepancies are found, report success and confirm the system is working correctly
