# Review Plan then Next Steps

Run `/review-plan` followed by `/next-steps` in sequence. This is the chained version used by `/implement-phase`.

## Arguments
- `$ARGUMENTS` - Description of what was implemented (passed to both sub-skills)

## Instructions

### Step 1: Review

**Invoke `/review-plan` using the Skill tool:**

```
Skill: review-plan
Args: $ARGUMENTS
```

If the review finds issues, fix them and re-invoke `/review-plan` until clean.

### Step 2: Next Steps

**After the review passes, invoke `/next-steps` using the Skill tool:**

```
Skill: next-steps
Args: $ARGUMENTS
```

### Step 3: Report

After both complete, report the combined results.

## Context

$ARGUMENTS
